import { el, api, jsonWithin } from './lib.js';

export function experimentPanel(owner) {
  let pending = false, epoch = 0;
  const content = el('div');
  const panel = el('section', {}, el('h2', {}, 'Experiments'),
    el('p', { class: 'sub' }, 'Measure a baseline, branch hypotheses and compare saved results. Selecting a winner keeps your working files unchanged.'));
  const render = (...nodes) => content.replaceChildren(...nodes.filter(node => node != null));
  const input = (label, value = '', max = 4096) => el('textarea', { 'aria-label': label, maxlength: max }, value);
  const button = (label, action) => el('button', { onclick: () => request(typeof action === 'function' ? action() : action) }, label);
  const request = async action => {
    if (pending) return;
    try {
      if (action.error) throw new Error(action.error);
      if (!jsonWithin(action, 65536)) throw new Error('Request exceeds 64 KiB. Shorten the hypothesis, parameters or contract.');
      pending = true; const generation = ++epoch;
      render(el('p', {}, 'Reading experiment…'));
      const reply = await api(`/api/sessions/${encodeURIComponent(owner)}/experiments`, action);
      if (generation !== epoch || !panel.isConnected) return;
      if (reply.items) tree(reply);
      else if (reply.rows) comparison(reply);
      else if (reply.token) {
        const prepared = { node: reply.node, token: reply.token };
        render(el('p', {}, 'Run this saved source and parameters with the fixed command?'),
          el('pre', {}, `${reply.command}\n${JSON.stringify(reply.parameters, null, 2)}`),
          button('Run saved experiment', { action: 'run', ...prepared, wait: false }),
          button('Back to node', { action: 'inspect', node: reply.node }));
      } else if (reply.text !== undefined) {
        const read = { action: 'read', node: reply.node, run: reply.run, artifact: reply.artifact };
        render(el('pre', {}, reply.text), reply.next != null ? button('Read next page', { ...read, offset: reply.next }) : null,
          button('Back to node', { action: 'inspect', node: reply.node }));
      } else if (reply.runs) show(reply);
      else render(el('pre', {}, JSON.stringify(reply, null, 2)),
        reply.node ? button('Inspect run', { action: 'inspect', node: reply.node }) : button('Refresh tree', { action: 'list' }));
    } catch (error) {
      if (panel.isConnected) render(el('p', { class: 'warn' }, String(error.error || error)), button('Refresh tree', { action: 'list' }));
    } finally { pending = false; }
  };
  const parsed = (field, fallback = {}) => {
    try { return JSON.parse(field.value || JSON.stringify(fallback)); }
    catch { throw new Error('Parameters and contract must be valid JSON.'); }
  };
  const form = parent => {
    const title = input('Experiment title', '', 128), hypothesis = input('Experiment hypothesis');
    const parameters = input('Experiment parameters', parent ? JSON.stringify(parent.parameters) : '{}', 32768);
    const contract = input('Experiment contract', '{"command":"./evaluate.sh","metrics":{"score":"maximize"},"artifacts":[]}', 16384);
    const action = () => {
      try { return { action: parent ? 'branch' : 'create', ...(parent ? { parent: parent.id } : { contract: parsed(contract) }),
        title: title.value, hypothesis: hypothesis.value, parameters: parsed(parameters) }; }
      catch (error) { return { error: error.message }; }
    };
    return el('div', {}, title, hypothesis, parameters,
      parent ? null : el('div', {}, el('p', {}, 'The fixed command reads .rook-experiment/parameters.json and writes declared numeric metrics to .rook-experiment/metrics.json. Artifact paths belong under .rook-experiment/artifacts/.'), contract),
      button(parent ? 'Create child hypothesis' : 'Create baseline', action));
  };
  const tree = reply => {
    const nodes = reply.items || [];
    const depths = new Map();
    render(...nodes.map(node => {
      const depth = node.parent ? (depths.get(node.parent) || 0) + 1 : 0; depths.set(node.id, depth);
      return button(`${'  '.repeat(depth)}${node.parent ? '└ ' : ''}${node.title} · ${node.answered ? 'measured' : 'unanswered'}`, { action: 'inspect', node: node.id });
    }), nodes.length ? button('Compare current baseline', { action: 'compare', baseline: reply.winner || reply.baseline }) : form(null));
  };
  const show = node => {
    const note = input('Experiment recovery inspection note');
    render(el('h3', {}, node.title), el('p', {}, node.hypothesis),
      el('pre', {}, JSON.stringify(node.parameters, null, 2)),
      node.error ? el('p', { class: 'warn' }, node.error) : null,
      el('p', { class: 'sub' }, `Saved source: ${node.source}`),
      el('p', { class: 'sub' }, `Candidate session: ${node.session}`),
      node.answered ? form(node) : el('div', {},
        el('p', {}, 'Edit the candidate workspace, then save its source before evaluating.'),
        el('pre', {}, node.workspace),
        button('Save candidate source', { action: 'capture', node: node.id }),
        button('Prepare run', { action: 'prepare', node: node.id })),
      ...node.runs.map(run => el('div', {}, el('p', {}, `${run.id} · ${run.state}${run.live ? ' · running' : run.uncertain ? ' · result uncertain' : ''}`),
        el('p', { class: 'sub' }, `Run session: ${run.session}`),
        run.error ? el('p', { class: 'warn' }, run.error) : null,
        run.metrics ? el('pre', {}, JSON.stringify(run.metrics, null, 2)) : null,
        run.output ? button('Read run output', { action: 'read', node: node.id, run: run.id }) : null,
        ...Object.keys(run.artifacts || {}).map(path => button(`Read ${path}`, { action: 'read', node: node.id, run: run.id, artifact: path })),
        run.live ? button('Stop run', { action: 'stop', node: node.id, run: run.id }) : null,
        run.uncertain && !run.live ? el('div', {}, el('p', {}, 'Inspect the run session and acknowledge unknown execution effects before recording recovery.'), note,
          button('Record inspected recovery', () => ({ action: 'recover', node: node.id, run: run.id, note: note.value }))) : null)),
      node.answered ? button('Compare child hypotheses', { action: 'compare', baseline: node.id }) : null,
      button('Refresh node', { action: 'inspect', node: node.id }));
  };
  const comparison = reply => render(el('p', {}, `Baseline: ${reply.baseline}`),
    ...reply.rows.map(row => el('div', {}, el('h3', {}, row.title), el('pre', {}, JSON.stringify(row.deltas || row.runs, null, 2)),
      row.promotable && reply.baseline === reply.selection_baseline ? button('Select measured winner', { action: 'promote', node: row.node, token: reply.token }) : null,
      button('Inspect hypothesis', { action: 'inspect', node: row.node }))));
  panel.append(button('Refresh tree', { action: 'list' }), content);
  return panel;
}
