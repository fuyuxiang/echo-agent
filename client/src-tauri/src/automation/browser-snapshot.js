(() => {
          globalThis.__echoSnapshotDocumentToken ||= `${Date.now()}-${Math.random()}`;
          const visible = (el) => {
            const s = getComputedStyle(el), r = el.getBoundingClientRect();
            return s.visibility !== 'hidden' && s.display !== 'none' && r.width > 0 && r.height > 0;
          };
          const selectors = [
            'a[href]','button','input','textarea','select','summary','details',
            '[role="button"]','[role="link"]','[role="checkbox"]','[role="radio"]',
            '[role="tab"]','[role="menuitem"]','[contenteditable="true"]','[tabindex]'
          ].join(',');
          let seq = Number(document.documentElement.dataset.echoRefSeq || 0);
          const nodes = [];
          for (const el of globalThis.__echoQueryAll(selectors)) {
            if (!visible(el) || el.closest('[aria-hidden="true"]')) continue;
            if (!el.dataset.echoRef) el.dataset.echoRef = `e${++seq}`;
            const r = el.getBoundingClientRect();
            const inputType = (el.getAttribute('type') || '').toLowerCase();
            const safeValue = inputType === 'password' ? '' : (el.value || '');
            const labelledBy = (el.getAttribute('aria-labelledby') || '').split(/\s+/)
              .filter(Boolean).map(id => el.getRootNode().getElementById?.(id)?.textContent || '')
              .filter(text => text.trim()).join(' ').trim();
            const associatedLabel = Array.from(el.labels || [])
              .map(label => label.innerText || label.textContent || '').join(' ').trim();
            const label = (labelledBy || el.getAttribute('aria-label') || associatedLabel || el.getAttribute('title') ||
              el.getAttribute('placeholder') || el.innerText || safeValue).trim().slice(0, 300);
            nodes.push({
              ref: el.dataset.echoRef,
              tag: el.tagName.toLowerCase(),
              role: el.getAttribute('role') || '',
              name: label,
              type: inputType,
              disabled: !!el.disabled || el.getAttribute('aria-disabled') === 'true',
              checked: typeof el.checked === 'boolean' ? el.checked : undefined,
              href: el.href || undefined,
              value: inputType === 'password' ? undefined : String(safeValue).slice(0, 300),
              bounds: { x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height) }
            });
            if (nodes.length >= 500) break;
          }
          document.documentElement.dataset.echoRefSeq = String(seq);
          return {
            documentToken: globalThis.__echoSnapshotDocumentToken,
            url: location.href,
            title: document.title,
            viewport: { width: innerWidth, height: innerHeight, scrollX, scrollY },
            elements: nodes,
            text: (document.body?.innerText || '').replace(/\s+/g, ' ').trim().slice(0, 12000)
          };
        })()
