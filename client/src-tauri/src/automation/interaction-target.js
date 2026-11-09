// Evaluated in a CDP isolated world, separate from website scripts. The weak
// map keeps actual node identity stable without marking or retaining the DOM.
(input, documentToken, scroll, destinationOnly, clickPoint, formGuard) => {
  const state = globalThis.__echoInteractionTargets ??= {
    documentToken,
    nodes: new WeakMap(),
    next: 0,
  };
  const actionable = 'a[href],button,input,textarea,select,summary,label,[role],[contenteditable],[tabindex]';
  const nodeId = node => {
    if (!state.nodes.has(node)) state.nodes.set(node, ++state.next);
    return state.nodes.get(node);
  };
  const implicitInputTypes = new Set(['text', 'search', 'tel', 'url', 'email', 'password',
    'date', 'month', 'week', 'time', 'datetime-local', 'number']);
  const attribute = (element, name) => Element.prototype.getAttribute.call(element, name);
  const isSubmitter = element => (element.localName === 'button' && element.type === 'submit') ||
    (element.localName === 'input' && ['submit', 'image'].includes(element.type));
  const describeForm = (form, submitter) => {
    const override = (name, fallback) => (submitter && attribute(submitter, `form${name}`)) ?? fallback;
    const rawAction = override('action', attribute(form, 'action'));
    const rawMethod = (override('method', attribute(form, 'method')) || '').toLowerCase();
    const method = ['post', 'dialog'].includes(rawMethod) ? rawMethod : 'get';
    let action = null;
    if (method !== 'dialog') {
      try {
        const url = rawAction?.trim() ? new URL(rawAction, document.baseURI) : new URL(document.URL);
        action = ['http:', 'https:'].includes(url.protocol) && !url.username && !url.password ? url.href : null;
      } catch {
        // Rust refuses to authorize a network submission without a valid URL.
      }
    }
    return {
      form: nodeId(form), submitter: submitter ? nodeId(submitter) : null,
      disabled: submitter?.matches(':disabled') ?? false, action, method,
      target: override('target', attribute(form, 'target')) || document.querySelector('base[target]')?.target || '_self',
      encoding: override('enctype', attribute(form, 'enctype')),
      noValidate: attribute(form, 'novalidate') !== null || (submitter ? attribute(submitter, 'formnovalidate') !== null : false),
    };
  };
  const describeSubmission = (element, implicit) => {
    const form = element.form;
    if (!form) return null;
    if (isSubmitter(element)) return describeForm(form, element);
    if (!implicit || element.localName !== 'input' || !implicitInputTypes.has(element.type)) return null;
    // The default button is the first submitter in tree order, including
    // controls associated by the form attribute and image buttons.
    const submitter = Array.from(document.querySelectorAll('button,input'))
      .find(candidate => candidate.form === form && isSubmitter(candidate)) || null;
    if (!submitter && Array.from(document.querySelectorAll('input')).filter(candidate =>
      candidate.form === form && implicitInputTypes.has(candidate.type)).length > 1) return null;
    return describeForm(form, submitter);
  };
  const finishFormGuard = () => {
    const guard = state.formGuard;
    if (!guard) return false;
    clearTimeout(guard.timer);
    guard.controller.abort();
    state.formGuard = null;
    return guard.blocked;
  };
  if (formGuard === 'finish') return finishFormGuard();
  const armFormGuard = submission => {
    if (!formGuard || !submission) return;
    const matches = actual => Object.keys(formGuard).every(key => formGuard[key] === actual[key]);
    if (!matches(submission)) return;
    finishFormGuard();
    const guard = { controller: new AbortController(), blocked: false };
    const check = event => {
      if (event.defaultPrevented || !(event.target instanceof HTMLFormElement)) return;
      // Click handlers run before the submit event and form handlers can also
      // change the destination. Check both phases while dispatching input.
      if (!matches(describeForm(event.target, event.submitter))) {
        event.preventDefault();
        guard.blocked = true;
      }
    };
    const signal = guard.controller.signal;
    addEventListener('submit', check, { capture: true, signal });
    addEventListener('submit', check, { signal });
    guard.timer = setTimeout(finishFormGuard, 30000);
    state.formGuard = guard;
  };
  const describe = (element, hit = element) => {
    if (!element || !hit) return null;
    // The top document cannot verify the actual recipient inside a frame or
    // a separate tree. Do not approve its host as if it were the recipient.
    if (hit.closest('iframe,frame,object,embed') || hit.shadowRoot ||
        hit.localName.includes('-')) return null;
    const labelled = (element.getAttribute('aria-labelledby') || '').split(/\s+/)
      .map(id => document.getElementById(id)?.textContent || '').join(' ').trim();
    const label = Array.from(element.labels || [])
      .map(item => item.innerText || item.textContent || '').join(' ').trim();
    const tag = element.localName;
    const role = element.getAttribute('role') || '';
    const text = element.matches('input,textarea,[contenteditable]') || element.isContentEditable
      ? '' : element.innerText;
    const name = (labelled || element.getAttribute('aria-label') || label ||
      element.getAttribute('title') || element.getAttribute('placeholder') ||
      (element.matches(actionable) ? text : '') || role || tag).trim().slice(0, 300);
    return {
      node: nodeId(element),
      hit: nodeId(hit),
      preview: { tag, role, name, type: (element.getAttribute('type') || '').toLowerCase(),
        href: element.matches('a[href]') ? element.href : null,
        disabled: !!element.disabled || element.getAttribute('aria-disabled') === 'true' },
    };
  };
  const at = (x, y, reference = null) => {
    const hit = document.elementFromPoint(x, y);
    const nearest = hit?.closest(actionable);
    if (reference && (!hit || !reference.contains(hit) ||
        (nearest && nearest !== reference && reference.contains(nearest)))) return null;
    return describe(reference || nearest || hit, hit);
  };
  const clickControl = (x, y) => {
    const control = document.elementFromPoint(x, y)?.closest('a[href],button,input,textarea,select,summary,label');
    return control?.localName === 'label' ? control.control : control;
  };
  const context = { documentToken: state.documentToken, url: location.href };
  switch (input.kind) {
    case 'click': {
      let { x, y } = input;
      let reference = null;
      if (input.reference !== null) {
        const candidates = document.querySelectorAll('[data-echo-ref=' + CSS.escape(input.reference) + ']');
        if (candidates.length !== 1) return null;
        reference = candidates[0];
        if (scroll) reference.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
        if (!clickPoint) {
          const bounds = reference.getBoundingClientRect();
          x = bounds.left + bounds.width / 2;
          y = bounds.top + bounds.height / 2;
        }
      }
      if (clickPoint) ({ x, y } = clickPoint);
      const target = at(x, y, reference);
      if (!target) return null;
      const control = clickControl(x, y);
      const submission = control && (!input.button || input.button === 'left') ? describeSubmission(control, false) : null;
      armFormGuard(submission);
      return { ...context, target, submission, x, y };
    }
    case 'key': {
      const active = document.activeElement;
      if (input.reference && active?.getAttribute('data-echo-ref') !== input.reference) return null;
      const target = active && !active.shadowRoot && describe(active);
      if (!target) return null;
      const key = input.key?.toLowerCase();
      const submission = key === 'enter' || key === 'return' ? describeSubmission(active, true) :
        key === 'space' || key === ' ' ? describeSubmission(active, false) : null;
      armFormGuard(submission);
      return { ...context, target, submission };
    }
    case 'drag': {
      const target = destinationOnly ? null : at(input.fromX, input.fromY);
      const destination = at(input.toX, input.toY);
      return destination && (destinationOnly || target) ? { ...context, target, destination } : null;
    }
  }
  return null;
}
