// Fixed WK driver for an explicitly isolated Coding workspace journey. All
// edits below represent user typing in the real Monaco buffer; model results
// are never inserted, tool callbacks are never mocked, and disk is never written.
((scope) => {
  if (!scope || typeof scope.home !== 'string' || typeof scope.nonce !== 'string' || scope.nonce.length < 24
    || window.parent === window || location.protocol !== 'http:'
    || !['127.0.0.1', 'localhost', '[::1]'].includes(location.hostname)) return;
  let bootstrap;
  try { bootstrap = window.name.startsWith('echo-embed:') ? JSON.parse(window.name.slice(11)) : null; } catch { return; }
  if (!bootstrap?.parentOrigin || !bootstrap.bridgeToken) return;
  const parentOrigin = bootstrap.parentOrigin;
  const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
  const resolvedServices = {};
  const inlineConfiguration = [];
  const browserErrors = [];
  window.addEventListener('error',event=>browserErrors.push(String(event.message).slice(0,1000)));
  window.addEventListener('unhandledrejection',event=>browserErrors.push(String(event.reason).slice(0,1000)));
  let editor = null;
  let busy = false;
  let root = null;
  const focusAttempts = [];
  const declaredMember = (value, name, kind = 'method') => {
    const seen = new Set();
    for (let current = value; current && !seen.has(current); current = Object.getPrototypeOf(current)) {
      seen.add(current);
      const descriptor = Object.getOwnPropertyDescriptor(current, name);
      if (descriptor) return kind === 'getter'
        ? typeof descriptor.get === 'function' : typeof descriptor.value === 'function';
    }
    return false;
  };
  const service = (name) => {
    const container = window.theia?.container;
    if (!container?._bindingDictionary?.traverse) throw new Error('Theia container is not ready');
    // Production webpack minifies class/identifier names. Read only already
    // instantiated real services by their stable public API; do not instantiate
    // arbitrary bindings, replace a binding, or provide a fixture service.
    const matches = new Set();
    const inspected = new Set();
    const rejectedIntrospection = new Set();
    const seenContainers = new Set();
    for (let current = container; current && !seenContainers.has(current); current = current.parent) {
      seenContainers.add(current);
      current._bindingDictionary?.traverse((_key, bindings) => {
        for (const binding of bindings) {
          const value = binding.cache;
          if (!value || typeof value !== 'object' || inspected.has(value)) continue;
          inspected.add(value);
          try {
            const workspace = name === 'WorkspaceService'
              && declaredMember(value, 'tryGetRoots') && declaredMember(value, 'getWorkspaceRootUri')
              && declaredMember(value, 'isWorkspaceFile') && declaredMember(value, 'roots', 'getter')
              && declaredMember(value, 'onWorkspaceChanged', 'getter') && typeof value.onWorkspaceChanged === 'function';
            const manager = name === 'EditorManager'
              && declaredMember(value, 'open') && declaredMember(value, 'getByUri')
              && declaredMember(value, 'onCurrentEditorChanged') && declaredMember(value, 'onActiveEditorChanged')
              // editor-preview is enabled in the production browser and
              // rebinds EditorManager to its singleton subclass. Both IDs
              // come from actual bundled factories; uniqueness is required.
              && ['code-editor-opener', 'editor-preview-widget'].includes(value.id);
            const bridge = name === 'EchoEditorBridge'
              && declaredMember(value,'contextSnapshot')&&declaredMember(value,'documentSnapshot')
              && declaredMember(value,'applyOperation')&&declaredMember(value,'whenIdle');
            if (workspace || manager || bridge) matches.add(value);
          } catch {
            // Some unrelated cached preference/RPC proxies reject reflection
            // (including revoked proxies and invalid descriptor traps). They
            // are not safely declared services. Exclude them, without catching
            // errors from the selected real service's roots or editor.open.
            rejectedIntrospection.add(value);
          }
        }
      });
    }
    if (matches.size !== 1) throw new Error(`Theia cached service ${name} has ${matches.size} matching instances; ${rejectedIntrospection.size} candidates rejected unsafe introspection`);
    const value = [...matches][0];
    resolvedServices[name] = { constructorName: value.constructor?.name, matchingInstances: matches.size,
      rejectedIntrospectionCandidates: rejectedIntrospection.size,
      resolvedFrom: 'actual container cache and stable public interfaces' };
    return value;
  };
  const waitFor = async (check, timeout = 30000) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      const result = await check();
      if (result) return result;
      await delay(100);
    }
    throw new Error('Timed out waiting for the actual Theia editor');
  };

  const control = () => {
    if (!editor) throw new Error('A real editor has not been opened');
    const value = editor.editor.getControl();
    if (!value.getModel()) throw new Error('The real editor has no model');
    return value;
  };
  const state = () => ({
    text: control().getModel().getValue(), version: control().getModel().getVersionId(),
    dirty: editor.saveable?.dirty ?? editor.editor.document.dirty,
    path: editor.editor.uri.path.fsPath(), selection: control().getSelection(),
    visible: editor.node.getClientRects().length > 0, resolvedServices,
  });
  const escape = () => {
    const node=control().getDomNode();
    const target=node.querySelector('textarea.inputarea')??document.activeElement;
    if(!target||!node.contains(target))throw new Error('Actual Monaco keyboard input is unavailable');
    target.dispatchEvent(new KeyboardEvent('keydown', {
      key:'Escape',code:'Escape',keyCode:27,which:27,bubbles:true,cancelable:true
    }));
  };
  const inputNodeDiagnostics = node => {
    if(!node)return null;
    const style=getComputedStyle(node),rect=node.getBoundingClientRect();
    return {tag:node.tagName,classes:node.className,isConnected:node.isConnected,disabled:node.disabled===true,
      readOnly:node.readOnly===true,tabIndex:node.tabIndex,contentEditable:node.contentEditable,
      inertAncestor:!!node.closest('[inert]'),display:style.display,visibility:style.visibility,
      rect:{x:rect.x,y:rect.y,width:rect.width,height:rect.height}};
  };
  const inlineDiagnostics = () => {
    const input=control(),manager=service('EditorManager');
    const action=input.getAction('editor.action.inlineSuggest.trigger');
    let controller,controllerError,bridge,bridgeError;
    try{controller=input.getContribution('editor.contrib.inlineCompletionsController');}catch(error){controllerError=String(error).slice(0,1000);}
    try{bridge=service('EchoEditorBridge');}catch(error){bridgeError=String(error).slice(0,1000);}
    return {path:state().path,version:input.getModel().getVersionId(),uri:input.getModel().uri.toString(),
      actionPresent:!!action,actionSupported:action?.isSupported(),controllerPresent:!!controller,controllerError,
      controllerModelPresent:!!controller?.model?.get?.(),hasTextFocus:input.hasTextFocus(),
      documentHasFocus:document.hasFocus(),activeElement:{tag:document.activeElement?.tagName,classes:document.activeElement?.className},
      editorNode:inputNodeDiagnostics(editor.node),controlNode:inputNodeDiagnostics(input.getDomNode()),layout:input.getLayoutInfo(),
      inputNodes:[...input.getDomNode().querySelectorAll('textarea.inputarea,.native-edit-context,[contenteditable="true"]')].slice(0,4).map(inputNodeDiagnostics),
      focusAttempts:focusAttempts.slice(-6),
      browserEditContextType:typeof window.EditContext,experimentalEditContext:input.getRawOptions().experimentalEditContext,
      currentEditorMatches:manager.currentEditor===editor,documentModelMatches:editor.editor.document.textEditorModel===input.getModel(),
      selectionEmpty:input.getSelection()?.isEmpty(),readOnly:input.getRawOptions().readOnly,
      inlineSuggest:input.getRawOptions().inlineSuggest,bridgePresent:!!bridge,bridgeError,
      providerPresent:!!bridge?.inline,providerEnabled:bridge?.inline?.enabled,
      configurations:inlineConfiguration.slice(-10),browserErrors:browserErrors.slice(-10)};
  };
  const open = async (filename, requireTextFocus=true) => {
    if (!['double.cjs', 'completion.cjs','other.cjs'].includes(filename)) throw new Error('Unsupported fixed fixture file');
    await waitFor(() => window.theia?.container && document.querySelector('#theia-app-shell'));
    const roots = await service('WorkspaceService').roots;
    const expected=`${scope.home}/workspaces/${filename==='other.cjs'?'other':'default'}`;
    if (roots.length !== 1 || roots[0].resource.path.fsPath() !== expected) {
      throw new Error('The real IDE is outside the fixed private workspace');
    }
    root = roots[0].resource.path.fsPath();
    editor = await service('EditorManager').open(roots[0].resource.resolve(filename));
    await waitFor(() => editor?.editor?.getControl()?.getModel());
    const requiresInlineFocus=filename==='completion.cjs';
    if(requiresInlineFocus) {
      await waitFor(()=>{
        const node=control().getDomNode(),rect=node?.getBoundingClientRect(),layout=control().getLayoutInfo();
        return service('EditorManager').currentEditor===editor&&editor.node.isConnected&&node?.isConnected
          &&rect.width>0&&rect.height>0&&layout.width>0&&layout.height>0;
      });
      await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
      window.focus();
    }
    control().focus();
    if(requiresInlineFocus) {
      focusAttempts.push({source:'control.focus after actual attachment, visibility and layout',hasTextFocus:control().hasTextFocus(),
        documentHasFocus:document.hasFocus(),activeElement:inputNodeDiagnostics(document.activeElement)});
      if(!control().hasTextFocus()) {
        const input=control().getDomNode().querySelector('textarea.inputarea');
        if(input?.isConnected&&!input.disabled&&!input.closest('[inert]')) {
          input.focus({preventScroll:true});
          focusAttempts.push({source:'ordinary HTMLElement.focus on the real attached Monaco input',hasTextFocus:control().hasTextFocus(),
            documentHasFocus:document.hasFocus(),activeElement:inputNodeDiagnostics(document.activeElement),input:inputNodeDiagnostics(input)});
        }
      }
      if(requireTextFocus)await waitFor(()=>service('EditorManager').currentEditor===editor&&control().hasTextFocus()&&document.hasFocus());
    }
  };
  window.addEventListener('message', event => {
    if(event.source===window.parent&&event.origin===parentOrigin&&event.data?.type==='echo/set-inline-completion'
      &&event.data.token===bootstrap.bridgeToken) {
      inlineConfiguration.push({enabled:event.data.enabled,instanceId:event.data.instanceId});
    }
    if (event.source !== window.parent || event.origin !== parentOrigin || !event.data
      || event.data.type !== 'echo/native-workspace-operation' || event.data.nonce !== scope.nonce
      || typeof event.data.id !== 'string' || busy) return;
    const message = event.data;
    busy = true;
    void (async () => {
      try {
        let value;
        if (message.action === 'open') await open('double.cjs');
        else if(message.action==='tab_focus_target') {
          await open('completion.cjs',false);
          const input=control().getDomNode().querySelector('textarea.inputarea');
          const measured=inputNodeDiagnostics(input);
          if(!input||!measured.isConnected||measured.disabled||measured.inertAncestor||measured.display==='none'||measured.visibility!=='visible'
            ||measured.rect.width<=0||measured.rect.height<=0||!document.hasFocus())throw new Error('Native input target is not a real visible editor input');
          value={path:state().path,input:measured.rect,childViewport:{width:innerWidth,height:innerHeight},
            focused:control().hasTextFocus()&&document.activeElement===input,diagnostics:inlineDiagnostics()};
        }
        else if(message.action==='open_other')await open('other.cjs');
        else if(message.action==='restored') {
          await waitFor(()=>window.theia?.container&&document.querySelector('#theia-app-shell'));
          const roots=await service('WorkspaceService').roots;
          if(roots.length!==1||roots[0].resource.path.fsPath()!==`${scope.home}/workspaces/default`)throw new Error('Production project switch did not return to the owned root');
          root=roots[0].resource.path.fsPath();
          await waitFor(()=>{editor=service('EditorManager').currentEditor;return editor?.editor?.getControl()?.getModel();});
        }
        else if(message.action==='diff') {
          const diff=await waitFor(()=>{const current=service('EditorManager').currentEditor;return current?.editor?.diffEditor?.getModel()?current.editor.diffEditor:null;});
          const model=diff.getModel();
          const node=[...document.querySelectorAll('.monaco-diff-editor')].find(element=>element.getClientRects().length>0);
          if(!node)throw new Error('Actual Monaco diff is not visible');
          value={visible:true,original:model.original.getValue(),modified:model.modified.getValue(),
            originalUri:model.original.uri.toString(),modifiedUri:model.modified.uri.toString(),
            originalReadOnly:diff.getOriginalEditor().getRawOptions().readOnly===true,
            modifiedVersion:model.modified.getVersionId()};
        }
        else if (message.action === 'dirty') {
          if (!state().path.endsWith('/double.cjs') || state().text !== 'exports.double = n => n + 1;\n') throw new Error('Unexpected original user buffer');
          control().pushUndoStop();
          control().executeEdits('native-user-typing', [{range:control().getModel().getFullModelRange(),
            text:'exports.double = n => n + 1;\n// UNSAVED_A01\n', forceMoveMarkers:true}]);
          control().pushUndoStop();
          control().setSelection({startLineNumber:1,startColumn:1,endLineNumber:2,endColumn:15});
          await waitFor(() => state().dirty === true);
        } else if (message.action === 'save') {
          const saveable = editor.saveable ?? editor.editor.document;
          if (typeof saveable.save !== 'function') throw new Error('The real editor does not support save');
          await saveable.save(); await waitFor(() => !state().dirty);
        } else if (message.action === 'tab_setup' || message.action === 'tab_prepare') {
          await open('completion.cjs');
          const model = control().getModel();
          // Fixed user prefix only. The missing continuation must come from the
          // production inline provider and configured remote model.
          control().pushUndoStop();
          control().executeEdits('native-user-prefix', [{range:model.getFullModelRange(),
            text:'function triple(n) {\n  return n * \n}\n',forceMoveMarkers:true}]);
          control().pushUndoStop(); control().setPosition({lineNumber:2,column:14});
          if(message.action==='tab_setup')control().trigger('native-user', 'editor.action.inlineSuggest.trigger', undefined);
        } else if(message.action==='inline_diagnostics') {
          value=inlineDiagnostics();
        } else if(message.action==='tab_trigger') {
          control().trigger('native-user','editor.action.inlineSuggest.trigger',undefined);
        } else if (message.action === 'tab_accept') {
          if (!state().path.endsWith('/completion.cjs')) throw new Error('No private completion editor');
          const input=control().getDomNode().querySelector('textarea.inputarea');
          if(!input||document.activeElement!==input||!control().hasTextFocus())throw new Error('Real Tab keyboard input is not focused');
          input.dispatchEvent(new KeyboardEvent('keydown', {
            key:'Tab',code:'Tab',keyCode:9,which:9,bubbles:true,cancelable:true
          }));
          await delay(150);
        } else if (message.action === 'tab_cancel') {
          control().trigger('native-user', 'editor.action.inlineSuggest.hide', undefined);
        } else if (message.action === 'tab_escape') {
          escape();
        } else if (message.action === 'type_during_completion') {
          control().trigger('keyboard', 'type', {text:'/* A05 */ '});
          control().trigger('native-user', 'editor.action.inlineSuggest.trigger', undefined);
          await delay(200);
          control().trigger('keyboard', 'type', {text:'3'});
          await delay(100);
        } else if (message.action === 'type_after_inline_error') {
          if (!state().path.endsWith('/completion.cjs')) throw new Error('No isolated inline editor after error');
          control().trigger('keyboard', 'type', {text:'/* A17_STILL_TYPING */'});
          escape();
          await delay(100);
        } else if (message.action === 'undo' || message.action === 'redo') {
          control().trigger('native-user',message.action,null); await delay(150);
        } else if (message.action !== 'state') throw new Error('Unsupported fixed workspace operation');
        window.parent.postMessage({type:'echo/native-workspace-result',nonce:scope.nonce,id:message.id,ok:true,value:value??state()},parentOrigin);
      } catch (error) {
        window.parent.postMessage({type:'echo/native-workspace-result',nonce:scope.nonce,id:message.id,ok:false,error:String(error)},parentOrigin);
      } finally {busy=false;}
    })();
  });
})
