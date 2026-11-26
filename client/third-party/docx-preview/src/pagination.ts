/*
 * Reflow pagination.
 *
 * The renderer only splits pages at explicit breaks (breakPages option);
 * content that Word would flow across multiple physical pages ends up in a
 * single overgrown <section>. This module is a post-render DOM pass (opt-in
 * via the `paginate` option) that re-packs each section's article children
 * into page-sized sections, splitting tables by row and paragraphs by line.
 *
 * It runs after the rendered nodes are attached to a container, so all
 * measurements (offsetHeight, Range rects) reflect real layout. Callers must
 * ensure webfonts are settled first (document.fonts.ready), otherwise split
 * points are computed against fallback-font metrics.
 */

const FIT_EPSILON = 1; // px of slack for sub-pixel rounding
const MAX_PAGES_PER_SECTION = 1000; // runaway-loop backstop

export function paginateWrapper(container: HTMLElement, className: string): number {
	const sections = Array.from(container.querySelectorAll(`section.${className}`)) as HTMLElement[];
	let pageCount = 0;

	for (const section of sections) {
		pageCount += paginateSection(section, className);
	}

	if (pageCount > 0) {
		// Continuation fragments produced by splitting a paragraph must not
		// repeat its first-line indent or its list number.
		const style = document.createElement("style");
		style.textContent =
			`.${className} .${className}-continuation { text-indent: 0 !important; }` +
			`.${className} .${className}-continuation::before { content: none !important; }`;
		container.prepend(style);

		// Pages created by splitting share the original page's header/footer,
		// whose PAGE/NUMPAGES fields were resolved before the split — renumber
		// across the final page list.
		const pages = container.querySelectorAll(`section.${className}`);
		const total = `${pages.length}`;
		pages.forEach((page, i) => {
			for (const el of Array.from(page.querySelectorAll(`.${className}-field-page`)))
				el.textContent = `${i + 1}`;
			for (const el of Array.from(page.querySelectorAll(`.${className}-field-numpages`)))
				el.textContent = total;
		});
	}

	return pageCount;
}

function paginateSection(section: HTMLElement, className: string): number {
	const article = section.querySelector(":scope > article") as HTMLElement;
	if (!article) return 0;

	const sectionStyle = getComputedStyle(section);
	const pageHeight = parseFloat(sectionStyle.minHeight);
	if (!pageHeight || Number.isNaN(pageHeight)) return 0; // no fixed page height to paginate against

	const columns = getComputedStyle(article).columnCount;
	if (columns !== "auto" && columns !== "1") return 0; // multi-column layouts can't be split reliably

	const contentLimit = pageHeight - parseFloat(sectionStyle.paddingTop) - parseFloat(sectionStyle.paddingBottom);
	if (!(contentLimit > 0) || article.scrollHeight <= contentLimit + FIT_EPSILON) return 0;

	const header = section.querySelector(":scope > header");
	const footer = section.querySelector(":scope > footer");
	// Footnote/endnote lists belong to the section; keep them on the last page.
	const notes = Array.from(section.querySelectorAll(":scope > ol"));

	const pages: HTMLElement[] = [section]; // the original section becomes page 1
	let currentArticle = article;
	let lastPage = section;

	function newPage(): void {
		const shell = section.cloneNode(false) as HTMLElement;
		if (header) shell.appendChild(header.cloneNode(true));
		const pageArticle = article.cloneNode(false) as HTMLElement;
		shell.appendChild(pageArticle);
		if (footer) shell.appendChild(footer.cloneNode(true));
		// Attach immediately: packing the page requires live layout
		// (scrollHeight/Range rects are all zero on detached nodes).
		lastPage.after(shell);
		lastPage = shell;
		pages.push(shell);
		currentArticle = pageArticle;
	}

	const fits = () => currentArticle.scrollHeight <= contentLimit + FIT_EPSILON;
	const bottomLimit = () => currentArticle.getBoundingClientRect().top + contentLimit;

	const blocks = Array.from(article.children) as HTMLElement[];
	for (const b of blocks) b.remove();

	for (let i = 0; i < blocks.length && pages.length < MAX_PAGES_PER_SECTION; i++) {
		const block = blocks[i];
		currentArticle.appendChild(block);
		if (fits()) continue;

		// Overflow: try to split the block at the page boundary.
		const tail = splitBlock(block, bottomLimit(), className);
		if (tail === "allfit") continue; // benign box overshoot — keep it here
		if (tail) {
			blocks.splice(i + 1, 0, tail);
			newPage();
			continue;
		}
		if (tail) {
			blocks.splice(i + 1, 0, tail);
			newPage();
			continue;
		}

		block.remove();
		if (currentArticle.childElementCount === 0) {
			// A single block taller than a whole page: place it and let it
			// overflow rather than loop forever.
			currentArticle.appendChild(block);
			continue;
		}

		newPage();
		i--; // retry the block on a fresh page
	}

	// Attach section-level notes to the last page (before its footer).
	if (notes.length > 0 && pages.length > 1) {
		const lastFooter = lastPage.querySelector(":scope > footer");
		for (const ol of notes) lastPage.insertBefore(ol, lastFooter);
	}

	if (pages.length <= 1) return 0;

	return pages.length;
}

/**
 * Splits `block` (already appended to a page and known to overflow) so its
 * head fits above `bottomLimit` (viewport coordinate). Returns the detached
 * tail fragment, "allfit" when the block's content doesn't actually
 * overflow (keep it on the current page), or null when the block can't
 * usefully be split here.
 */
function splitBlock(block: HTMLElement, bottomLimit: number, className: string): HTMLElement | "allfit" | null {
	if (block.tagName === "TABLE") {
		return splitTable(block as HTMLTableElement, bottomLimit);
	}

	const pos = findTextSplitOffset(block, bottomLimit);
	if (pos === "allfit") return "allfit";
	if (pos === null) return null;

	const tail = splitAt(block, pos.node, pos.offset);
	tail.classList.add(`${className}-continuation`);
	return tail;
}

function splitTable(table: HTMLTableElement, bottomLimit: number): HTMLElement | null {
	const bodyRows: HTMLTableRowElement[] = [];
	for (const tbody of Array.from(table.tBodies)) {
		bodyRows.push(...Array.from(tbody.rows));
	}
	if (bodyRows.length < 2) return null;

	const splitIndex = bodyRows.findIndex(r => r.getBoundingClientRect().bottom > bottomLimit);
	if (splitIndex <= 0) return null; // no row fits in the remaining space

	const tail = table.cloneNode(false) as HTMLTableElement;
	// Keep column widths and header rows on the continuation table.
	const colgroup = table.querySelector(":scope > colgroup");
	if (colgroup) tail.appendChild(colgroup.cloneNode(true));
	if (table.tHead) tail.appendChild(table.tHead.cloneNode(true));

	for (let i = splitIndex; i < bodyRows.length; i++) {
		tail.appendChild(bodyRows[i]);
	}

	return tail as unknown as HTMLElement;
}

/**
 * Finds the latest text position inside `el` that still renders above
 * `bottomLimit`, by walking text nodes in order and binary-searching each
 * one with Range rects. Returns:
 * - { node, offset } — split here;
 * - "allfit" — every text node fits; the overflow comes from the block's
 *   own box (min-height overshoot), not its content, so it should stay;
 * - null — even the first character overflows (or there is no text).
 */
function findTextSplitOffset(el: HTMLElement, bottomLimit: number): { node: Text, offset: number } | "allfit" | null {
	const doc = el.ownerDocument;
	const walker = doc.createTreeWalker(el, NodeFilter.SHOW_TEXT);
	const range = doc.createRange();
	let hasFittingContent = false;
	let sawText = false;

	for (let node = walker.nextNode() as Text; node; node = walker.nextNode() as Text) {
		if (node.length === 0) continue;
		sawText = true;

		let lo = 0, hi = node.length, fit = 0;
		while (lo <= hi) {
			const mid = (lo + hi) >> 1;
			range.setStart(node, 0);
			range.setEnd(node, mid);
			const rects = range.getClientRects();
			const last = rects[rects.length - 1];
			if (last && last.bottom <= bottomLimit) {
				fit = mid;
				lo = mid + 1;
			} else {
				hi = mid - 1;
			}
		}

		if (fit === 0) {
			// Nothing of this node fits. Splitting before it only makes sense
			// when earlier content already filled the page.
			return hasFittingContent ? { node, offset: 0 } : null;
		}

		if (fit < node.length) {
			// Prefer breaking after whitespace so Latin words aren't cut in
			// half (CJK text may break anywhere, so this is best-effort).
			let back = fit;
			const floor = Math.max(0, fit - 30);
			while (back > floor && !/\s/.test(node.data[back - 1])) back--;
			return { node, offset: back > floor ? back : fit };
		}

		hasFittingContent = true;
	}

	return sawText ? "allfit" : null;
}

/**
 * Splits `el` at (textNode, offset): `el` keeps everything before the point,
 * and the returned element (a shallow clone of `el` with cloned inline
 * ancestors) gets everything from the point on.
 */
function splitAt(el: HTMLElement, textNode: Text, offset: number): HTMLElement {
	const tail = el.cloneNode(false) as HTMLElement;
	const secondHalf = textNode.splitText(offset);
	moveTail(secondHalf, el, tail);
	return tail;
}

/** Moves the split point's following content into a parallel clone
 * hierarchy under `tailRoot`: `node` and its following siblings go into a
 * clone of their parent, and each ancestor up to `root` contributes a clone
 * plus its following siblings. The ORIGINAL ancestors stay in the head —
 * moving them would rip the head's split-span text out (and reverse the
 * order of the two text halves in the tail). */
function moveTail(node: Node, root: HTMLElement, tailRoot: HTMLElement): void {
	let child = node;
	let container = tailRoot;

	while (child.parentNode && child.parentNode !== root) {
		const parent = child.parentNode as HTMLElement;
		const parentClone = parent.cloneNode(false) as HTMLElement;
		container.appendChild(parentClone);
		parentClone.appendChild(child);
		while (child.nextSibling) parentClone.appendChild(child.nextSibling);
		child = parent;
		container = parentClone;
	}

	// `child` is now a direct child of root. If the loop never ran, `node`
	// itself was that direct child and still needs moving; otherwise its
	// clone is already in place and only following siblings move over.
	if (child === node) tailRoot.appendChild(child);
	while (child.nextSibling) tailRoot.appendChild(child.nextSibling);
}
