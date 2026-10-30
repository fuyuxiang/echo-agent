// Modified by EchoAgent for the embedded desktop runtime; see ECHOAGENT_VENDOR.md.
mod debug_redact;
pub mod find_protoc;

use anyhow::Context;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn dependency_paths<'a>(
    content: &'a str,
    descriptor_path: &str,
) -> anyhow::Result<impl Iterator<Item = &'a str>> {
    // Match the complete target before the separator: Windows drive letters
    // contain a colon, and the temporary directory may contain spaces.
    let dependencies = content
        .strip_prefix(descriptor_path)
        .and_then(|rest| rest.strip_prefix(':'))
        .context("unexpected protoc dependency_out target")?;
    Ok(dependencies
        .lines()
        .map(|line| line.trim().strip_suffix('\\').unwrap_or(line.trim()).trim())
        .filter(|line| !line.is_empty()))
}

/// Find the protoc well-known types include directory.
///
/// When PROTOC is set (e.g., in Bazel), the include directory is typically
/// at `../include` relative to the `bin/protoc` binary. For example:
/// - PROTOC = `/path/to/external/protoc_linux_x86_64/bin/protoc`
/// - Include = `/path/to/external/protoc_linux_x86_64/include`
///
/// This is needed because Bazel places the protoc binary and include files
/// in separate locations within the sandbox, and protoc doesn't automatically
/// find them without an explicit -I flag.
fn find_protoc_include_dir(protoc: Option<&Path>) -> Option<PathBuf> {
    let protoc = protoc?;

    // protoc is typically at .../bin/protoc, so include is at .../include
    let parent = protoc.parent()?; // .../bin
    let grandparent = parent.parent()?; // .../
    let include_dir = grandparent.join("include");

    if include_dir.is_dir() {
        Some(include_dir)
    } else {
        None
    }
}

pub struct EchoAgentProtoBuilder {
    builder: tonic_prost_build::Builder,
    file_descriptor_set_path: Option<PathBuf>,
    gen_pbjson: bool,
    pbjson_ignore_unknown_fields: bool,
    pbjson_preserve_proto_field_names: bool,
    pbjson_exclude: Vec<String>,
    honor_debug_redact: bool,
}

impl EchoAgentProtoBuilder {
    fn map_builder(
        self,
        f: impl FnOnce(tonic_prost_build::Builder) -> tonic_prost_build::Builder,
    ) -> Self {
        Self {
            builder: f(self.builder),
            ..self
        }
    }

    pub fn btree_map<S: AsRef<str>>(self, paths: impl IntoIterator<Item = S>) -> Self {
        self.map_builder(|b| paths.into_iter().fold(b, |b, path| b.btree_map(path)))
    }

    pub fn bytes<S: AsRef<str>>(self, paths: impl IntoIterator<Item = S>) -> Self {
        self.map_builder(|b| paths.into_iter().fold(b, |b, path| b.bytes(path)))
    }

    pub fn extern_path(self, proto_path: impl AsRef<str>, rust_path: impl AsRef<str>) -> Self {
        self.map_builder(|b| b.extern_path(proto_path, rust_path))
    }

    pub fn file_descriptor_set_path(mut self, path: impl AsRef<Path>) -> Self {
        self.file_descriptor_set_path = Some(path.as_ref().to_path_buf());
        self.map_builder(|b| b.file_descriptor_set_path(path))
    }

    pub fn gen_pbjson(mut self) -> Self {
        self.gen_pbjson = true;
        self
    }

    pub fn pbjson_ignore_unknown_fields(mut self) -> Self {
        self.pbjson_ignore_unknown_fields = true;
        self
    }

    /// Serialize JSON using the original proto field names (snake_case) instead
    /// of the proto3-JSON default (camelCase). Deserialization still accepts
    /// both casings, so this is backward-compatible with already-stored
    /// camelCase documents.
    pub fn pbjson_preserve_proto_field_names(mut self) -> Self {
        self.pbjson_preserve_proto_field_names = true;
        self
    }

    /// Skip pbjson serde generation for these fully-qualified proto type
    /// prefixes (e.g. `.model_config.RateLimit`). Use when a type is
    /// `extern_path`'d into another crate that already provides its pbjson serde
    /// impls, but the enclosing package's serde is still generated here —
    /// otherwise pbjson would emit an orphan `impl Serialize for <foreign type>`.
    /// Matching is segment-based, so `.pkg.Foo` does not match `.pkg.FooBar`.
    pub fn pbjson_exclude<S: Into<String>>(
        mut self,
        prefixes: impl IntoIterator<Item = S>,
    ) -> Self {
        self.pbjson_exclude
            .extend(prefixes.into_iter().map(Into::into));
        self
    }

    pub fn generate_default_stubs(self, enable: bool) -> Self {
        self.map_builder(|b| b.generate_default_stubs(enable))
    }

    /// Honor the protobuf `debug_redact` field option: annotated fields
    /// print as `***` in `Debug`. The crate must also depend on `veil`.
    pub fn honor_debug_redact(mut self) -> Self {
        self.honor_debug_redact = true;
        self
    }

    pub fn type_attribute(self, path: impl AsRef<str>, attr: impl AsRef<str>) -> Self {
        self.map_builder(|b| b.type_attribute(path, attr))
    }

    pub fn field_attribute(self, path: impl AsRef<str>, attr: impl AsRef<str>) -> Self {
        self.map_builder(|b| b.field_attribute(path, attr))
    }

    // tonic-build generation of `rerun-if-changed` is lazy and incorrect.
    // - everything is invalidated when anything inside include directories is changed
    // - also they compute paths incorrectly: assuming paths are relative to current directory
    //   rather than
    fn emit_rerun_if_changed<'a>(
        protoc: Option<&Path>,
        protoc_include_dir: Option<&Path>,
        protos: impl IntoIterator<Item = &'a Path>,
        includes: impl IntoIterator<Item = &'a Path>,
    ) -> anyhow::Result<()> {
        let includes = Vec::from_iter(includes);

        if let Some(protoc) = protoc {
            println!(
                "cargo:rerun-if-changed={}",
                protoc.to_str().context("protoc path not UTF-8")?
            );
        }

        // Can only process one input file when using --dependency_out=FILE.
        for proto in protos {
            // protoc's Windows path handling can turn `NUL` into a real file
            // in the crate directory. Keep both outputs in a temporary
            // directory, cleaned up on success and error, with no open handles
            // that could prevent protoc from writing them on Windows.
            let temp_dir = tempfile::tempdir()
                .context("failed to create temp directory for protoc dependencies")?;
            let dep_path = temp_dir.path().join("dependencies.d");
            let descriptor_path = temp_dir.path().join("descriptor.pbbin");
            let descriptor_path = descriptor_path
                .to_str()
                .context("descriptor temp path not UTF-8")?;
            let mut command = Command::new(protoc.unwrap_or(Path::new("protoc")));
            command
                .arg(format!(
                    "--dependency_out={}",
                    dep_path.to_str().context("dep temp path not UTF-8")?
                ))
                .arg(format!("--descriptor_set_out={descriptor_path}"));

            // Add protoc's well-known types include directory first (if found).
            // This is needed for Bazel sandboxed builds where protoc and its
            // include files are in different locations.
            if let Some(include_dir) = protoc_include_dir {
                command.arg(format!(
                    "-I{}",
                    include_dir.to_str().context("include path not UTF-8")?
                ));
            }

            for include in &includes {
                command.arg(format!("-I{}", include.to_str().context("path not UTF-8")?));
            }

            command.arg(proto);

            command.stdin(Stdio::null());
            command.stderr(Stdio::inherit());

            let output = command.output().context("protoc command failed")?;
            if !output.status.success() {
                return Err(anyhow::anyhow!("protoc command failed"));
            }

            // EchoAgent patch: read dependency list from the temp file we
            // passed via --dependency_out (instead of parsing stdout).
            let dep_content = fs::read_to_string(&dep_path)
                .context("failed to read protoc dependency_out temp file")?;
            for line in dependency_paths(&dep_content, descriptor_path)? {
                // Depending on absolute paths like
                // /Users/user/homebrew/Cellar/protobuf/29.1/include/google/protobuf/timestamp.proto
                // is valid, but we want to have output more deterministic.
                if line.contains("/include/google/protobuf/") {
                    continue;
                }

                if !fs::exists(line)? {
                    return Err(anyhow::anyhow!("dependency file not found: {line}"));
                }

                println!("cargo:rerun-if-changed={line}");
            }
        }

        Ok(())
    }

    pub fn compile_protos(
        self,
        protos: &[impl AsRef<Path>],
        includes: &[impl AsRef<Path>],
    ) -> anyhow::Result<()> {
        for proto in protos {
            let proto = proto.as_ref();
            if proto.is_absolute() {
                return Err(anyhow::anyhow!(
                    "Absolute paths are not allowed: {}",
                    proto.display()
                ));
            }
        }

        let EchoAgentProtoBuilder {
            builder,
            gen_pbjson,
            file_descriptor_set_path,
            pbjson_ignore_unknown_fields,
            pbjson_preserve_proto_field_names,
            pbjson_exclude,
            honor_debug_redact,
        } = self;
        let mut config = prost_build::Config::new();
        config.enable_type_names();

        let protoc = find_protoc::find_protoc()?;

        // Use fixed version of `protoc` binary.
        if let Some(protoc) = &protoc {
            config.protoc_executable(protoc);
        }

        // Find the protoc's well-known types include directory.
        // This is needed for Bazel sandboxed builds where protoc and its
        // include files are placed in different sandbox locations.
        let protoc_include_dir = find_protoc_include_dir(protoc.as_deref());

        let mut builder = builder.emit_rerun_if_changed(false);
        Self::emit_rerun_if_changed(
            protoc.as_deref(),
            protoc_include_dir.as_deref(),
            protos.iter().map(|p| p.as_ref()),
            includes.iter().map(|i| i.as_ref()),
        )?;

        let tempfile;

        let file_descriptor_set_path: Option<PathBuf> =
            if let Some(file_descriptor_set_path) = file_descriptor_set_path {
                Some(file_descriptor_set_path)
            } else if gen_pbjson {
                tempfile = tempfile::TempDir::new()?;
                let file_descriptor_set_path = tempfile.path().join("echo-agent-proto-build.pbbin");
                builder = builder.file_descriptor_set_path(&file_descriptor_set_path);
                Some(file_descriptor_set_path)
            } else {
                None
            };

        // Build the full includes list, prepending the protoc include directory
        // if found (for well-known types like google/protobuf/timestamp.proto).
        let all_includes: Vec<&Path> = protoc_include_dir
            .as_deref()
            .into_iter()
            .chain(includes.iter().map(|i| i.as_ref()))
            .collect();

        let protos: Vec<&Path> = protos.iter().map(|p| p.as_ref()).collect();

        {
            let plain_includes: Vec<&Path> = includes.iter().map(|i| i.as_ref()).collect();
            if honor_debug_redact {
                debug_redact::apply(
                    &mut config,
                    protoc.as_deref(),
                    protoc_include_dir.as_deref(),
                    &plain_includes,
                    &protos,
                )?;
            } else if let Some(field) = debug_redact::first_marked_field(
                protoc.as_deref(),
                protoc_include_dir.as_deref(),
                &plain_includes,
                &protos,
            )? {
                anyhow::bail!(
                    "{field} sets `debug_redact = true` but redaction is not active: \
                     call `.honor_debug_redact()` on the builder"
                );
            }
        }

        builder
            .compile_with_config(config, &protos, &all_includes)
            .context("tonic_build failed")?;

        if gen_pbjson {
            let file_descriptor_set_path =
                file_descriptor_set_path.context("fds must be set at this moment")?;
            let descriptor_set = fs::read(&file_descriptor_set_path).with_context(|| {
                format!(
                    "Failed to read file descriptor set {}",
                    file_descriptor_set_path.display()
                )
            })?;
            let mut builder = pbjson_build::Builder::new();
            builder
                .register_descriptors(&descriptor_set)
                .context("Failed to register descriptors in pbjson_build")?;
            if pbjson_ignore_unknown_fields {
                builder.ignore_unknown_fields();
            }
            if pbjson_preserve_proto_field_names {
                builder.preserve_proto_field_names();
            }
            if !pbjson_exclude.is_empty() {
                builder.exclude(pbjson_exclude);
            }
            builder
                .build(&["."])
                .context("Failed to build descriptor set")?;
        }

        Ok(())
    }
}

pub fn configure() -> EchoAgentProtoBuilder {
    let builder = tonic_prost_build::configure()
        .compile_well_known_types(true)
        .extern_path(".google.protobuf", "::pbjson_types")
        .extern_path(".google.protobuf.Empty", "()")
        .protoc_arg("--experimental_allow_proto3_optional");
    EchoAgentProtoBuilder {
        builder,
        gen_pbjson: false,
        pbjson_ignore_unknown_fields: false,
        pbjson_preserve_proto_field_names: false,
        pbjson_exclude: Vec::new(),
        file_descriptor_set_path: None,
        honor_debug_redact: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_paths_preserve_windows_drive_letters_and_spaces() {
        let target = r"C:\Users\开发者\AppData\Local\Temp\proto build\descriptor.pbbin";
        let content =
            format!("{target}: C:\\source tree\\shared.proto\\\r\n C:\\source tree\\main.proto");
        assert_eq!(
            dependency_paths(&content, target)
                .unwrap()
                .collect::<Vec<_>>(),
            [r"C:\source tree\shared.proto", r"C:\source tree\main.proto"]
        );
    }

    #[test]
    fn dependency_paths_match_the_complete_target() {
        let target = "/tmp/proto: build/descriptor.pbbin";
        let content = format!("{target}: shared.proto\\\n main.proto\n");
        assert_eq!(
            dependency_paths(&content, target)
                .unwrap()
                .collect::<Vec<_>>(),
            ["shared.proto", "main.proto"]
        );
        assert!(dependency_paths(&content, "/tmp/other.pbbin").is_err());
        assert!(dependency_paths("", target).is_err());
    }

    #[test]
    fn protoc_dependency_outputs_are_temporary_on_success_and_failure() {
        const CASE_ENV: &str = "ECHO_AGENT_PROTO_DEPENDENCY_TEST_CASE";
        if let Ok(case) = std::env::var(CASE_ENV) {
            let protoc = find_protoc::find_protoc().unwrap();
            let result = EchoAgentProtoBuilder::emit_rerun_if_changed(
                protoc.as_deref(),
                None,
                [Path::new("main.proto")],
                [Path::new(".")],
            );
            if case == "success" {
                result.unwrap();
            } else {
                assert!(result.is_err());
            }
            return;
        }

        let protoc = find_protoc::find_protoc()
            .unwrap()
            .expect("protoc must be installed for the dependency regression test");
        let protoc = std::path::absolute(protoc).unwrap();
        for (case, source) in [
            (
                "success",
                "syntax = \"proto3\"; import \"shared.proto\"; message Main { Shared value = 1; }",
            ),
            ("failure", "syntax = \"proto3\"; this is invalid protobuf"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace with spaces");
            let scratch = root.path().join("compiler output with spaces");
            fs::create_dir(&workspace).unwrap();
            fs::create_dir(&scratch).unwrap();
            fs::write(workspace.join("main.proto"), source).unwrap();
            fs::write(
                workspace.join("shared.proto"),
                "syntax = \"proto3\"; message Shared { string value = 1; }",
            )
            .unwrap();

            // Isolate cwd and temp environment changes in a child process.
            // On Windows this also catches a real NUL file in the workspace.
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::protoc_dependency_outputs_are_temporary_on_success_and_failure",
                    "--nocapture",
                ])
                .current_dir(&workspace)
                .env(CASE_ENV, case)
                .env("PROTOC", &protoc)
                .env("TMPDIR", &scratch)
                .env("TMP", &scratch)
                .env("TEMP", &scratch)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let mut files = fs::read_dir(&workspace)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            files.sort();
            assert_eq!(files, ["main.proto", "shared.proto"], "{case}");
            assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0, "{case}");
        }
    }
}
