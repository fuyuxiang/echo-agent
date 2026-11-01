/** Keep user-selected avatar swatches; choose readable initials on hex colors. */
export function avatarTextColor(background: string): string {
  // These strong semantic accents are dark in light mode and bright in dark
  // mode. Keep the pairing in CSS so a theme change updates both immediately.
  if (/^var\(--echo-(?:brand|status-(?:info|warning|purple))\)$/.test(background)) {
    return "var(--echo-bg-primary)";
  }
  const hex = background.replace(/^#/, "");
  if (!/^(?:[\da-f]{3}|[\da-f]{6})$/i.test(hex)) return "var(--echo-text-strong)";
  const full = hex.length === 3 ? [...hex].map(c => c + c).join("") : hex;
  const channels = [0, 2, 4].map(i => parseInt(full.slice(i, i + 2), 16) / 255)
    .map(c => c <= .04045 ? c / 12.92 : ((c + .055) / 1.055) ** 2.4);
  const luminance = channels.reduce((sum, c, i) => sum + c * [.2126, .7152, .0722][i]!, 0);
  return (luminance + .05) / .05 >= 1.05 / (luminance + .05)
    ? "var(--echo-palette-black-100)" : "var(--echo-palette-white-100)";
}
