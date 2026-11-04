import { describe, it, expect } from 'vitest';
import { assertPrimaryAction } from './ui-primary-action-audit.mjs';

describe('canonical primary-action audit', () => {
  it('accepts the documented opaque palette and still measures label contrast', () => {
    const result = assertPrimaryAction({background:'rgb(79, 91, 213)',color:'rgb(255, 255, 255)'},{theme:'light'});
    expect(result.contrast).toBeGreaterThanOrEqual(4.5);
    expect(() => assertPrimaryAction({background:'color(srgb 0.8 0.8196078431372549 1)',color:'rgb(23, 27, 58)'},{theme:'dark',state:'pressed'})).not.toThrow();
  });
  it.each([
    ['obsolete mint', 'rgb(10, 128, 80)', 'rgb(255, 255, 255)'],
    ['one-channel drift', 'rgb(80, 91, 213)', 'rgb(255, 255, 255)'],
    ['wrong foreground', 'rgb(79, 91, 213)', 'rgb(250, 250, 250)'],
    ['transparent background', 'rgba(79, 91, 213, 0.5)', 'rgb(255, 255, 255)'],
  ])('rejects %s rather than accepting a generic high-contrast colour', (_, background, color) => {
    expect(() => assertPrimaryAction({background,color},{theme:'light'})).toThrow();
  });
  it('rejects a valid palette in the wrong theme or interaction state', () => {
    const control={background:'rgb(79, 91, 213)',color:'rgb(255, 255, 255)'};
    expect(() => assertPrimaryAction(control,{theme:'dark'})).toThrow();
    expect(() => assertPrimaryAction(control,{theme:'light',state:'hover'})).toThrow();
  });
});
