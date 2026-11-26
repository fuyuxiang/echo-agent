# Third-Party Notices

This crate contains code ported from, or derived from, the open-source
projects listed under [Ported source code](#ported-source-code), and its
release builds embed prebuilt third-party tool binaries listed under
[Bundled tool binaries](#bundled-tool-binaries). The original license terms
are reproduced below, as required by those licenses.

Ported files have been modified from their originals (translated between
languages, adapted to this crate's `Tool` trait and runtime, and extended);
this file constitutes the prominent notice of those changes required by
Apache License 2.0 §4(b).

## Ported source code

### openai/codex

The tool implementations under `src/implementations/codex/` (`apply_patch`,
`grep_files`, `list_dir`, `read_file`) are ported from the
[openai/codex](https://github.com/openai/codex) project
(`codex-rs/core/src/tools/handlers/`).

Copyright 2025 OpenAI

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.

### sst/opencode

The tool implementations under `src/implementations/opencode/` (`bash`,
`edit`, `glob`, `grep`, `read`, `skill`, `todowrite`, `write`) are ported
from the [sst/opencode](https://github.com/sst/opencode) project
(`packages/opencode/src/tool/`).

MIT License

Copyright (c) 2025 opencode

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## Bundled tool binaries

Release builds of this crate embed unmodified, prebuilt binaries of the
tools below (see `build.rs`); they are self-extracted to `~/.echo-agent/vendor/`
at runtime. Which tools are embedded in a given build depends on what the
release pipeline supplies at build time:

- **ripgrep** is embedded in every release build (downloaded from the
  official GitHub release, or supplied via `ECHO_AGENT_TOOLS_BUNDLE_RG_PATH`).
- **ugrep** and **bfs** are embedded only when the release pipeline supplies
  static binaries via `ECHO_AGENT_TOOLS_BUNDLE_UGREP_PATH` /
  `ECHO_AGENT_TOOLS_BUNDLE_BFS_PATH`. When those are unset the tools are not
  bundled, and the runtime instead resolves them from `~/.echo-agent/vendor/` or
  `$PATH` if the user has installed them. Their license terms are included
  below so that any build that does bundle them is covered.

### ripgrep

[ripgrep](https://github.com/BurntSushi/ripgrep) is dual-licensed under the
MIT License or the Unlicense, at the user's option. We redistribute the
official ripgrep release binaries and reproduce the MIT license below.

The official ripgrep release binaries statically link
[PCRE2](https://github.com/PCRE2Project/pcre2) (BSD-3-Clause with the PCRE2
exemption for binary library-like packages). Under that exemption, packages
that include ripgrep — and do not use PCRE2 independently — are not subject
to PCRE2's binary-redistribution notice condition, so no separate PCRE2
notice is reproduced here.

The MIT License (MIT)

Copyright (c) 2015 Andrew Gallant

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.

### ugrep

[ugrep](https://github.com/Genivia/ugrep) is licensed under the BSD 3-Clause
License. Note: when the release pipeline builds the static ugrep binary,
review what it statically links (e.g. PCRE2, zlib) and extend this notice if
needed.

BSD 3-Clause License

Copyright (c) 2023, Robert van Engelen, Genivia Inc.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

### bfs

[bfs](https://github.com/tavianator/bfs) is licensed under the Zero-Clause
BSD (0BSD) license, which imposes no notice-retention requirement; the
license is reproduced here as a courtesy.

Copyright © 2015-2025 Tavian Barnes <tavianator@tavianator.com> and the bfs
contributors

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH
REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY
AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT,
INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM
LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR
OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR
PERFORMANCE OF THIS SOFTWARE.
