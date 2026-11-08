import assert from 'node:assert/strict';

// Canonical product palette, documented in docs/theme-design.md.
// Exact values are independent of the rendered tokens so token drift fails.
const palette = {
  light: { default: [79,91,213], hover: [66,76,189], pressed: [54,63,159], foreground: [255,255,255] },
  dark: { default: [165,174,255], hover: [186,193,255], pressed: [204,209,255], foreground: [23,27,58] },
};
function rgb(value, label) {
  assert(typeof value === 'string' && /^(rgba?\(|color\(srgb )/.test(value), `${label}: expected a computed RGB colour`);
  const values = value.match(/[\d.]+/g).map(Number);
  assert(values.length === 3 || values.length === 4, `${label}: invalid computed colour`);
  assert.equal(values[3] ?? 1, 1, `${label}: primary colours must be opaque`);
  return values.slice(0,3).map(channel => Math.round(channel * (value.startsWith('color(srgb') ? 255 : 1)));
}
export function assertPrimaryAction({ background, color }, { theme, state = 'default', label = 'Primary action' }) {
  const expected = palette[theme];
  assert(expected?.[state], `${label}: unknown theme or control state`);
  const bg = rgb(background, label), fg = rgb(color, label);
  assert.deepEqual(bg, expected[state], `${label}: ${theme}/${state} primary background differs from the product palette`);
  assert.deepEqual(fg, expected.foreground, `${label}: primary foreground differs from the product palette`);
  const luminance = channels => channels.map(channel => {
    const c = channel / 255;
    return c <= .04045 ? c / 12.92 : ((c + .055) / 1.055) ** 2.4;
  }).reduce((sum, channel, index) => sum + channel * [.2126,.7152,.0722][index], 0);
  const levels = [luminance(bg), luminance(fg)].sort((a,b) => b-a);
  const contrast = (levels[0]+.05)/(levels[1]+.05);
  assert(contrast >= 4.5, `${label}: primary label contrast is below 4.5:1`);
  return { background: bg, foreground: fg, contrast };
}
