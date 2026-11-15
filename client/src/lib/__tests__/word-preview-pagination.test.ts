import { describe, expect, it } from 'vitest';
import { Document, PageBreak, PageOrientation, Packer, Paragraph, SectionType, TextRun } from 'docx';
import { renderAsync } from '@apollo-design/docx-preview';

async function pages(sections: ConstructorParameters<typeof Document>[0]['sections']) {
  const data = await Packer.toBuffer(new Document({ sections }));
  const body = document.createElement('div'), styles = document.createElement('div');
  await renderAsync(new Uint8Array(data), body, styles, { inWrapper: false, breakPages: true, paginate: false, ignoreLastRenderedPageBreak: false });
  return [...body.querySelectorAll<HTMLElement>('section.docx')];
}

describe('Word original section boundaries', () => {
  it('starts a default next-page section on its own page', async () => {
    const result = await pages([{ children: [new Paragraph('First section')] }, { children: [new Paragraph('Second section')] }]);
    expect(result).toHaveLength(2);
    expect(result[0].textContent).toBe('First section');
    expect(result[1].textContent).toBe('Second section');
  });
  it('retains continuous sections on the current page', async () => {
    const result = await pages([{ children: [new Paragraph('First section')] },
      { properties: { type: SectionType.CONTINUOUS }, children: [new Paragraph('Continuous section')] }]);
    expect(result).toHaveLength(1);
    expect(result[0].textContent).toContain('First sectionContinuous section');
  });
  it('honors an explicit break inside one section', async () => {
    const result = await pages([{ children: [new Paragraph('Page one'), new Paragraph({ children: [new PageBreak(), new TextRun('Page two')] })] }]);
    expect(result).toHaveLength(2);
    expect(result[1].textContent).toBe('Page two');
  });
  it('applies landscape geometry to the new section', async () => {
    const result = await pages([{ children: [new Paragraph('Portrait')] },
      { properties: { page: { size: { orientation: PageOrientation.LANDSCAPE } } }, children: [new Paragraph('Landscape')] }]);
    expect(result).toHaveLength(2);
    expect(parseFloat(result[0].style.width)).toBeLessThan(parseFloat(result[1].style.width));
    expect(result[1].textContent).toBe('Landscape');
  });
});
