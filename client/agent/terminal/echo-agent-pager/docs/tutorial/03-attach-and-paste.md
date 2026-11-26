# Attach Files, Images & Paste

The more precisely you point EchoAgent at the right context, the better the
result. Three ways to get things into the prompt:

## Mention files with `@`

Type `@` for a fuzzy file picker — line ranges work too:

```
@src/main.rs          attach a file
@src/main.rs:10-50    attach specific lines
@!.env                reach hidden files with @!
```

## Paste images

Paste a screenshot straight into the prompt: `Cmd+V` on macOS, `Ctrl+V` on
Linux, `Alt+V` on Windows. Great for error dialogs, designs, and diagrams.

## Run shell commands yourself

Type `!` on an empty prompt to run a shell command directly — the output
lands in the scrollback where EchoAgent can see it too.

*Go deeper: `/docs Getting Started`*
