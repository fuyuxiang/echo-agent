//! In-TUI `/doctor` report formatting.

use super::{
    DataControlFact, DiagnosticReport, FindingDisposition, NewlineFact, RuntimeFact, VoiceFacts,
};
use crate::clipboard::{ClipboardDelivery, NativeClipboardPreflight};
use crate::host::{DisplayServer, HostOs};

pub fn format_doctor(report: &DiagnosticReport) -> String {
    let facts = &report.facts;
    let mut out = String::new();
    out.push_str("Environment\n");
    out.push_str(&format!("  terminal     {}\n", facts.terminal));
    if let RuntimeFact::Available(xtversion) = &facts.xtversion {
        out.push_str(&format!("  xtversion    {xtversion}\n"));
    }
    out.push_str(&format!("  multiplexer  {}\n", facts.multiplexer));
    if let Some(byobu) = facts.byobu {
        out.push_str(&format!("  byobu        {byobu}\n"));
    }
    out.push_str(&format!(
        "  ssh          {}\n",
        if facts.ssh { "yes" } else { "no" }
    ));
    let color_level = match &facts.color.level {
        RuntimeFact::Available(level) => Some(*level),
        RuntimeFact::NoReply | RuntimeFact::Unavailable => None,
    };
    if let Some(color_level) = color_level {
        out.push_str(&format!("  color        {}\n", color_level.as_str()));
    }
    if color_level.is_some() && facts.color.available_themes.len() == facts.color.total_themes {
        out.push_str("  themes       all\n");
    } else if color_level.is_some() {
        let themes = facts
            .color
            .available_themes
            .iter()
            .map(|theme| theme.display_name())
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "  themes       {}/{}: {themes}\n",
            facts.color.available_themes.len(),
            facts.color.total_themes
        ));
    }
    if let Some(keyboard) = &facts.keyboard {
        let rescue = if keyboard.os == HostOs::Macos {
            "OS rescue active"
        } else {
            "OS rescue unavailable on this platform"
        };
        out.push_str(&format!(
            "  keyboard     {} ({rescue})\n",
            keyboard.modifier_delivery.label()
        ));
    }
    if let Some(newline) = &facts.newline {
        let detail = match newline {
            NewlineFact::Vte {
                version: Some(version),
            } => format!("VTE {version}; need >= 8200 for Shift+Enter"),
            NewlineFact::Vte { version: None } => {
                "legacy VTE; need VTE >= 0.82 for Shift+Enter".to_owned()
            }
            NewlineFact::XtermJs { terminal } => {
                format!("{terminal}: xterm.js can't distinguish Shift+Enter")
            }
            NewlineFact::NoKittyKeyboardProtocol => {
                "no Kitty keyboard protocol; Shift+Enter == Enter".to_owned()
            }
        };
        out.push_str(&format!("  newline      Alt+Enter ({detail})\n"));
    }

    let clipboard = &facts.clipboard;
    let native = match clipboard.native_preflight {
        NativeClipboardPreflight::LocalAvailable => {
            format!("local ({})", clipboard.native_tool)
        }
        NativeClipboardPreflight::RemoteOnly if clipboard.container_no_display => {
            format!("container ({})", clipboard.native_tool)
        }
        NativeClipboardPreflight::RemoteOnly => {
            format!("remote ({})", clipboard.native_tool)
        }
        NativeClipboardPreflight::Unavailable => "unavailable".to_owned(),
        NativeClipboardPreflight::Disabled => "off".to_owned(),
    };
    out.push_str("\nClipboard\n");
    out.push_str(&format!("  native       {native}\n"));
    out.push_str(&format!(
        "  tmux         {}\n",
        if clipboard.tmux_route { "on" } else { "off" }
    ));
    out.push_str(&format!(
        "  osc 52       {}\n",
        if clipboard.osc52_route {
            clipboard.osc52_capability.label()
        } else {
            "off"
        }
    ));
    out.push_str(&format!(
        "  wrap         {}\n",
        if clipboard.wrap_sink { "on" } else { "off" }
    ));
    if clipboard.display_server == DisplayServer::Wayland {
        out.push_str(&format!(
            "  data-control {}\n",
            if clipboard.data_control == DataControlFact::Available {
                "on"
            } else {
                "off"
            }
        ));
    }
    let status = match clipboard.delivery {
        ClipboardDelivery::Confirmed => "confirmed",
        ClipboardDelivery::Unverified => "unverified",
        ClipboardDelivery::Failed => "unavailable",
    };
    out.push_str(&format!("  status       {status}\n"));

    if let Some(voice) = &facts.voice {
        out.push_str("\nVoice\n");
        match voice {
            VoiceFacts::Device { name, detail } => {
                out.push_str(&format!("  microphone   {name} ({detail})\n"));
            }
            VoiceFacts::Missing { .. } => {
                out.push_str("  microphone   none detected\n");
            }
        }
    }

    format_findings(report, &mut out);
    out
}

fn format_findings(report: &DiagnosticReport, out: &mut String) {
    let issues = report
        .findings
        .iter()
        .filter(|finding| finding.disposition == FindingDisposition::Issue)
        .collect::<Vec<_>>();
    if issues.is_empty() {
        if report.issue_count() == 0 {
            out.push_str("\nNo issues found.\n");
        } else {
            out.push_str("\nAn issue is shown in the Clipboard status above.\n");
        }
    } else {
        out.push_str(&format!("\nIssues ({})\n", issues.len()));
        for finding in issues {
            format_finding(out, finding);
        }
    }

    let recommendations = report
        .findings
        .iter()
        .filter(|finding| finding.disposition == FindingDisposition::Recommendation)
        .collect::<Vec<_>>();
    if !recommendations.is_empty() {
        out.push_str("\nRecommendations\n");
        for finding in recommendations {
            format_finding(out, finding);
        }
    }
}

fn format_finding(out: &mut String, finding: &super::DiagnosticFinding) {
    let marker = match finding.disposition {
        FindingDisposition::Issue => "!",
        FindingDisposition::Recommendation => "i",
    };
    out.push_str(&format!(
        "\n  {marker} {}  {}\n",
        finding.id, finding.message
    ));
    if let Some(automatic) = finding.automatic_remediation {
        let command = super::human_fix_command(automatic.fix_id)
            .unwrap_or_else(|| automatic.command.to_owned());
        out.push_str(&format!("      Automatic setup: `{command}`\n"));
    }
    if let Some(remediation) = &finding.remediation {
        match (&remediation.config_path, &finding.automatic_remediation) {
            (Some(path), _) => {
                out.push_str(&format!("      Add `{}` to {path}\n", remediation.fix));
            }
            (None, Some(_)) => {
                out.push_str(&format!("      One-off: `{}`\n", remediation.fix));
            }
            (None, None) => {
                out.push_str(&format!("      Run: `{}`\n", remediation.fix));
            }
        }
    }
    if let Some(note) = &finding.note {
        out.push_str(&format!("      Note: {note}\n"));
    }
}

#[cfg(test)]
#[path = "doctor_format_tests.rs"]
mod tests;
