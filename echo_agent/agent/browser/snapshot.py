"""Page snapshot: flatten a live page into ref-annotated text + locator map.

Playwright removed ``page.accessibility`` in 1.57 (deprecated since 1.24), so
the AX-tree walk this module used to do no longer exists on any supported
version. ``locator.aria_snapshot()`` is the sanctioned replacement, but it
yields YAML text only — no handles — so refs would still have to be re-resolved
through ``get_by_role(role, name=...).nth(k)``, which silently drifts whenever
two nodes share a role+name or the accessible name is computed differently than
the YAML rendered it.

Instead we run one DOM traversal in the page and return, for every interactive
node, an absolute positional XPath alongside its role/name. ``@eN`` then maps to
``frame.locator("xpath=...")`` — a single unambiguous element, no ordinal
guessing. The traversal is read-only: it does not tag or otherwise mutate the
DOM, so a snapshot can never perturb the page under test.
"""

from __future__ import annotations

from typing import Any

from loguru import logger

# Roles we hand a @eN ref to. Kept aligned with _SNAPSHOT_JS's roleOf() output.
INTERACTIVE_ROLES = frozenset({
    "button", "link", "textbox", "checkbox", "radio", "combobox", "listbox",
    "menuitem", "tab", "switch", "searchbox", "slider", "option", "spinbutton",
    "file", "clickable",
})

# Max frames traversed per snapshot. Ad-heavy pages carry dozens of iframes;
# walking all of them costs a round trip each and floods the snapshot budget.
MAX_FRAMES = 8

_SNAPSHOT_JS = r"""
() => {
  const INTERACTIVE_ROLE_ATTRS = new Set([
    'button','link','checkbox','radio','tab','switch','menuitem','menuitemcheckbox',
    'menuitemradio','option','slider','spinbutton','textbox','searchbox','combobox',
    'listbox','treeitem'
  ]);
  const SKIP_TAGS = new Set([
    'script','style','noscript','template','svg','canvas','head','meta','link','br','hr'
  ]);
  // A container that merely carries a click handler is only treated as one
  // interactive unit when it is small. Otherwise a page-wide onclick div would
  // collapse the whole document into a single unusable ref.
  const MAX_CLICKABLE_TEXT = 200;
  const MAX_NAME = 120;

  const trim = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const cut = (s, n) => (s.length > n ? s.slice(0, n) + '…' : s);

  function visible(el) {
    if (el.hidden) return false;
    if (el.getAttribute('aria-hidden') === 'true') return false;
    const style = el.ownerDocument.defaultView.getComputedStyle(el);
    if (!style) return true;
    if (style.display === 'none' || style.visibility === 'hidden') return false;
    if (style.opacity === '0') return false;
    const rect = el.getBoundingClientRect();
    // Zero-size is invisible unless it is a wrapper whose children have size.
    if (rect.width === 0 && rect.height === 0 && el.childElementCount === 0) return false;
    return true;
  }

  function roleOf(el) {
    const explicit = (el.getAttribute('role') || '').toLowerCase();
    if (INTERACTIVE_ROLE_ATTRS.has(explicit)) return explicit;
    const tag = el.tagName.toLowerCase();
    if (tag === 'a') return el.hasAttribute('href') ? 'link' : '';
    if (tag === 'button') return 'button';
    if (tag === 'summary') return 'button';
    if (tag === 'select') return el.multiple ? 'listbox' : 'combobox';
    if (tag === 'textarea') return 'textbox';
    if (tag === 'option') return 'option';
    if (tag === 'input') {
      const t = (el.type || 'text').toLowerCase();
      if (t === 'checkbox') return 'checkbox';
      if (t === 'radio') return 'radio';
      if (t === 'range') return 'slider';
      if (t === 'number') return 'spinbutton';
      if (t === 'file') return 'file';
      if (t === 'search') return 'searchbox';
      if (t === 'submit' || t === 'button' || t === 'reset' || t === 'image') return 'button';
      if (t === 'hidden') return '';
      return 'textbox';
    }
    if (explicit) return explicit;
    if (el.isContentEditable) return 'textbox';
    const tabindex = el.getAttribute('tabindex');
    const clickish = el.hasAttribute('onclick') ||
                     (tabindex !== null && tabindex !== '-1') ||
                     el.ownerDocument.defaultView.getComputedStyle(el).cursor === 'pointer';
    if (clickish && trim(el.innerText || '').length <= MAX_CLICKABLE_TEXT) return 'clickable';
    return '';
  }

  function labelText(el) {
    const id = el.getAttribute('id');
    if (id) {
      try {
        const lab = el.ownerDocument.querySelector('label[for="' + CSS.escape(id) + '"]');
        if (lab) return trim(lab.innerText || lab.textContent);
      } catch (e) { /* malformed id */ }
    }
    const wrapper = el.closest ? el.closest('label') : null;
    if (wrapper) return trim(wrapper.innerText || wrapper.textContent);
    return '';
  }

  function nameOf(el, role) {
    const aria = trim(el.getAttribute('aria-label'));
    if (aria) return aria;
    const labelledby = el.getAttribute('aria-labelledby');
    if (labelledby) {
      const parts = labelledby.split(/\s+/).map((rid) => {
        const target = el.ownerDocument.getElementById(rid);
        return target ? trim(target.innerText || target.textContent) : '';
      }).filter(Boolean);
      if (parts.length) return parts.join(' ');
    }
    if (role === 'textbox' || role === 'searchbox' || role === 'spinbutton' ||
        role === 'combobox' || role === 'listbox' || role === 'file' ||
        role === 'checkbox' || role === 'radio' || role === 'slider') {
      const lab = labelText(el);
      if (lab) return lab;
      const ph = trim(el.getAttribute('placeholder'));
      if (ph) return ph;
      const nm = trim(el.getAttribute('name'));
      if (nm) return nm;
    }
    const inner = trim(el.innerText || el.textContent);
    if (inner) return inner;
    const title = trim(el.getAttribute('title'));
    if (title) return title;
    const alt = trim(el.getAttribute('alt'));
    if (alt) return alt;
    if (el.tagName.toLowerCase() === 'input') {
      const t = (el.type || '').toLowerCase();
      if (t === 'submit' || t === 'button' || t === 'reset') return trim(el.value);
    }
    return '';
  }

  function xpathOf(el) {
    const parts = [];
    let cur = el;
    const doc = el.ownerDocument;
    while (cur && cur.nodeType === 1 && cur !== doc.documentElement) {
      let idx = 1;
      let sib = cur.previousElementSibling;
      while (sib) {
        if (sib.tagName === cur.tagName) idx++;
        sib = sib.previousElementSibling;
      }
      parts.unshift(cur.tagName.toLowerCase() + '[' + idx + ']');
      cur = cur.parentElement;
    }
    return '/html/' + parts.join('/');
  }

  function stateOf(el, role) {
    const flags = [];
    if (el.disabled || el.getAttribute('aria-disabled') === 'true') flags.push('disabled');
    if (el.readOnly) flags.push('readonly');
    if (role === 'checkbox' || role === 'radio' || role === 'switch') {
      const checked = el.checked !== undefined
        ? el.checked
        : el.getAttribute('aria-checked') === 'true';
      flags.push(checked ? 'checked' : 'unchecked');
    }
    if (el.getAttribute('aria-expanded') !== null) {
      flags.push('expanded=' + el.getAttribute('aria-expanded'));
    }
    if (el.required) flags.push('required');
    const tag = el.tagName.toLowerCase();
    if ((tag === 'input' || tag === 'textarea') && role !== 'checkbox' && role !== 'radio') {
      const t = (el.type || '').toLowerCase();
      // Never echo credential fields back into the model's context.
      if (t === 'password') {
        if (el.value) flags.push('value=***');
      } else if (el.value) {
        flags.push('value="' + cut(trim(el.value), 60) + '"');
      }
    }
    if (tag === 'select') {
      const opts = Array.from(el.options || []).slice(0, 20)
        .map((o) => trim(o.label || o.textContent)).filter(Boolean);
      if (opts.length) flags.push('options=[' + opts.join(', ') + ']');
      if (el.selectedIndex >= 0 && el.options[el.selectedIndex]) {
        flags.push('selected="' + trim(el.options[el.selectedIndex].textContent) + '"');
      }
    }
    return flags;
  }

  const entries = [];
  let budget = 4000;  // hard node cap; a runaway DOM must not hang the traversal

  function walk(el, depth) {
    if (budget <= 0 || depth > 60) return;
    const tag = el.tagName.toLowerCase();
    if (SKIP_TAGS.has(tag)) return;
    if (!visible(el)) return;
    // Nested frames are traversed separately by the Python side (each Playwright
    // Frame gets its own evaluate), so record a marker and stop here.
    if (tag === 'iframe' || tag === 'frame') {
      entries.push({ kind: 'frame', name: trim(el.getAttribute('title') || el.getAttribute('name')) });
      return;
    }
    const role = roleOf(el);
    if (role) {
      budget--;
      entries.push({
        kind: 'element',
        role: role,
        name: cut(nameOf(el, role), MAX_NAME),
        xpath: xpathOf(el),
        states: stateOf(el, role),
      });
      // select owns its options; anything else stops so a button's own label is
      // not repeated as a bare text line underneath it.
      return;
    }
    if (tag === 'img') {
      const alt = trim(el.getAttribute('alt'));
      if (alt) { budget--; entries.push({ kind: 'text', text: 'image: ' + cut(alt, MAX_NAME) }); }
      return;
    }
    if (/^h[1-6]$/.test(tag)) {
      const t = trim(el.innerText || el.textContent);
      if (t) { budget--; entries.push({ kind: 'heading', level: Number(tag[1]), text: cut(t, 300) }); }
      return;
    }
    for (const child of el.childNodes) {
      if (budget <= 0) return;
      if (child.nodeType === 3) {
        const t = trim(child.textContent);
        if (t) { budget--; entries.push({ kind: 'text', text: cut(t, 400) }); }
      } else if (child.nodeType === 1) {
        walk(child, depth + 1);
      }
    }
  }

  const root = document.body || document.documentElement;
  if (root) walk(root, 0);
  return {
    url: location.href,
    title: document.title || '',
    entries: entries,
  };
}
"""


def _render_entry(entry: dict[str, Any], ref: str | None) -> str:
    kind = entry.get("kind")
    if kind == "element":
        role = entry.get("role", "")
        name = entry.get("name", "")
        states = entry.get("states") or []
        suffix = f" [{' '.join(states)}]" if states else ""
        return f"[{ref}] {role} '{name}'{suffix}"
    if kind == "heading":
        level = entry.get("level", 1)
        return f"{'#' * int(level)} {entry.get('text', '')}"
    if kind == "frame":
        name = entry.get("name") or ""
        return f"(iframe{': ' + name if name else ''})"
    return str(entry.get("text", ""))


async def build_page_snapshot(
    page: Any, *, max_chars: int = 8000, max_frames: int = MAX_FRAMES
) -> tuple[str, dict[str, Any]]:
    """Return ``(ref-annotated text, {ref: locator})`` for *page*.

    Every interactive node gets a ``@eN`` ref backed by an absolute XPath
    locator scoped to the frame it was found in, so a ref resolves to exactly
    one element. Refs are renumbered on each snapshot — the model must always
    act on the most recent one.
    """
    frames = []
    try:
        frames = list(page.frames)[:max_frames]
    except Exception:
        frames = []
    if not frames:
        frames = [page]

    lines: list[str] = []
    ref_map: dict[str, Any] = {}
    counter = 0
    header_done = False

    for index, frame in enumerate(frames):
        try:
            payload = await frame.evaluate(_SNAPSHOT_JS)
        except Exception as e:
            # A frame can detach or be cross-origin-restricted mid-traversal;
            # skip it rather than losing the whole snapshot.
            logger.debug("snapshot evaluate failed on frame {}: {}", index, e)
            continue
        if not isinstance(payload, dict):
            continue
        if not header_done:
            title = payload.get("title") or ""
            url = payload.get("url") or ""
            if title or url:
                lines.append(f"Page: {title}".rstrip())
                lines.append(f"URL: {url}")
                lines.append("")
            header_done = True
        elif index > 0:
            frame_url = payload.get("url") or ""
            lines.append(f"--- iframe: {frame_url} ---")
        for entry in payload.get("entries") or []:
            if not isinstance(entry, dict):
                continue
            ref = None
            if entry.get("kind") == "element":
                counter += 1
                ref = f"@e{counter}"
                xpath = entry.get("xpath") or ""
                try:
                    ref_map[ref] = frame.locator(f"xpath={xpath}")
                except Exception as e:
                    logger.debug("locator build failed for {} ({}): {}", ref, xpath, e)
                    continue
            rendered = _render_entry(entry, ref)
            if rendered:
                lines.append(rendered)

    text = "\n".join(lines)
    if not text:
        text = "(页面无可提取内容)"
    if len(text) > max_chars:
        text = text[:max_chars] + "\n…(快照内容过长已截断，可用 scroll 或 evaluate 获取余下内容)"
    return text, ref_map
