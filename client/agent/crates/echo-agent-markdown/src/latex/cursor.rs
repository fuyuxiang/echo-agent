//! Byte cursor over TeX source.

/// Byte cursor over the TeX source.
pub(super) struct Cursor<'a> {
    pub(super) src: &'a str,
    pub(super) pos: usize,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    pub(super) fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    pub(super) fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    /// Consume `\command` (alphabetic name) or `\<single char>`; the leading
    /// backslash must already be consumed. Returns the command name.
    ///
    /// Unlike TeX we do NOT consume trailing whitespace: the caller's
    /// whitespace collapsing keeps `\to 0` rendering as `→ 0`.
    pub(super) fn read_command_name(&mut self) -> &'a str {
        let start = self.pos;
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() => {
                while matches!(self.peek(), Some(c) if c.is_ascii_alphabetic()) {
                    self.bump();
                }
                &self.src[start..self.pos]
            }
            Some(_) => {
                self.bump();
                &self.src[start..self.pos]
            }
            None => "",
        }
    }

    /// Skip whitespace (TeX collapses it; meaning comes from commands).
    pub(super) fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.bump();
        }
    }

    /// Read a balanced `{...}` group body, assuming `{` was already consumed.
    /// Returns the inner source (without braces). Unbalanced input returns
    /// the remainder of the source.
    pub(super) fn read_group_body(&mut self) -> &'a str {
        let start = self.pos;
        let mut depth = 1usize;
        while let Some(ch) = self.bump() {
            match ch {
                '\\' => {
                    // Skip escaped char so `\{`/`\}` don't affect depth.
                    self.bump();
                }
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &self.src[start..self.pos - 1];
                    }
                }
                _ => {}
            }
        }
        &self.src[start..self.pos]
    }

    /// Read the next "atom": a `{...}` group body, a `\command` (returned
    /// with backslash), or a single char. Skips leading whitespace.
    pub(super) fn read_atom(&mut self) -> Option<&'a str> {
        self.skip_ws();
        let start = self.pos;
        match self.peek()? {
            '{' => {
                self.bump();
                Some(self.read_group_body())
            }
            '\\' => {
                self.bump();
                self.read_command_name();
                Some(&self.src[start..self.pos])
            }
            _ => {
                self.bump();
                Some(&self.src[start..self.pos])
            }
        }
    }
}
