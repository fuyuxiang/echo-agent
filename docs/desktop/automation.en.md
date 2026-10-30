# Browser and computer automation

**Scope: Desktop.** Browser Use operates a controlled browser through page structure. Computer Use operates desktop applications through native controls and screen input. File generation, coding, and discussion can use their dedicated tools directly.

## Choose a capability

| Goal | Capability | Example |
| --- | --- | --- |
| Read websites, fill forms, download files | Browser Use | Research public sources |
| Operate local graphical applications and save dialogs | Computer Use | Create and save a text document |
| Create Word, Excel, PDF, or PowerPoint files | Office tools | [Document generation](knowledge-memory.md) |

## Browser Use

Chrome, Edge, or Chromium must be detected. Each task has its own browser profile and download directory, without automatically inheriting your usual browser login. Deleting an automation task cleans up its profile.

Specify the site, action, and output. Side effects require separate backend confirmation. Uploads are limited to ordinary files in an authorized workspace and require disclosure confirmation. Password fields are omitted from structural snapshots and cannot receive tool input.

The controlled browser permits public HTTP/HTTPS destinations by default. Enable intranet access explicitly for a task when needed. Revocation invalidates existing connections. A task proxy checks actual destinations and redirects.

## Computer Use

Describe the application, action, and result explicitly:

> In TextEdit, create a plain-text document containing “Meeting notes”, save it to the Desktop as “meeting-notes.txt”, and verify the saved content.

Use Notepad on Windows. Explicit requests activate Computer Use; the tool menu is another entry point. Native macOS Accessibility or Windows UI Automation controls are preferred, with screen-coordinate fallback where available.

| Platform | Conditions |
| --- | --- |
| macOS | Grant Accessibility and Screen Recording to the actual running application |
| Windows | Screenshot and input checks pass; an interactive desktop is available |
| Linux X11, underlying support | XTEST available; no official desktop release |
| Wayland, underlying support | Global screenshot and input injection disabled |

Locked screens, security surfaces, and protected controls may require user intervention. Capability is detected on the device. Interface support does not establish that every application scenario has passed physical-device validation.

## Execution and control

An explicitly authorized computer task runs continuously without step-by-step confirmation. Use “pause” or `/pause` and “continue saving” or `/resume`. Pausing, stopping, changing modes, and restarting revoke previous execution authorization. Restarted tasks require explicit resumption.

Task scope is execution context, not an OS filesystem sandbox. Verify the target window and final file or page state. Clicking Save is not sufficient verification.

Page text, native-control contents, and screenshots may be sent to the selected model. See [Data management](operations.md).
