import { el, api, state, jsonWithin } from './lib.js';

// Source IDs and the owner are captured by this panel; navigation never
// retargets a comment or an in-flight response to another conversation.
export function reviewPanel(owner, putDraft) {
  let epoch = 0;
  let active = null;
  const target = el('select', {}, ['working', 'commit', 'base'].map(value => el('option', { value }, value)));
  const revision = el('input', { maxlength: 256, placeholder: 'Commit or base revision' });
  const paths = el('textarea', { maxlength: 65536, placeholder: 'Optional exact file paths, one per line', 'aria-label': 'Selected paths' });
  const reviewId = el('input', { maxlength: 64, placeholder: 'Saved review id' });
  const content = el('div');
  const render = (...nodes) => content.replaceChildren(...nodes.filter(node => node != null));
  const panel = el('div', { class: 'card' }, el('h2', {}, 'Review changes'),
    el('p', { class: 'sub' }, 'Capture source once, review it independently, then attach comments to that saved version.'));
  const prepare = text => {
    if (String(state.chat.session) !== String(owner) && (state.chat.busy || state.chat.draft.trim())) {
      render(el('p', { class: 'empty' }, 'Another session has active work or a draft. Return to this review’s session before preparing feedback.'));
      return;
    }
    putDraft(owner, text);
  };
  const request = async action => {
    if (!jsonWithin(action, 65536)) {
      render(el('p', { class: 'empty' }, 'Review request exceeds 64 KiB; select fewer paths or shorten the comment.'));
      return;
    }
    const token = ++epoch;
    render(el('p', {}, 'Reading review…'));
    try {
      const reply = await api(`/api/sessions/${encodeURIComponent(owner)}/reviews`, action);
      if (token !== epoch || !panel.isConnected) return;
      if (reply.items) {
        render(...reply.items.map(item => el('button', {
          onclick: () => request({ action: 'inspect', review: item.id })
        }, `${item.id} · ${item.state}`)));
      } else if (reply.prompt) {
        prepare(reply.prompt);
      } else if (reply.text !== undefined) {
        render(el('p', { class: 'sub' }, `Saved source ${reply.source} · ${reply.path} · ${reply.side} · line ${reply.first_line} · byte ${reply.offset}`),
          el('pre', {}, reply.text), el('button', { onclick: () => request({ action: 'inspect', review: active.id }) }, 'Back to review'), reply.next != null ? el('button', {
            onclick: () => request({ action: 'source', review: active.id, path: reply.path, side: reply.side, offset: reply.next })
          }, 'Next source page') : null);
      } else {
        active = reply;
        reviewId.value = reply.id;
        show(reply);
      }
    } catch (error) {
      if (token === epoch && panel.isConnected) render(el('p', { class: 'empty' }, String(error.error || error)));
    }
  };
  const show = review => {
    const source = review.source;
    const files = el('select', { 'aria-label': 'Reviewed file' }, review.files.map(file => el('option', { value: file.path }, file.path)));
    const side = el('select', { 'aria-label': 'Source side' }, ['after', 'before'].map(value => el('option', { value }, value)));
    const line = el('input', { type: 'number', min: 1, max: 2147483647, value: 1, 'aria-label': 'Source line' });
    const text = el('textarea', { maxlength: 4096, placeholder: 'Comment on this exact saved source…' });
    render(el('p', { class: 'sub' }, `${review.id} · ${review.state} · source ${source}`),
      review.error ? el('p', {}, review.error) : null,
      el('button', { onclick: () => prepare(`Run review_changes with ${JSON.stringify({ review: review.id })} for this exact saved source in session ${owner}.`) }, 'Prepare independent review'),
      ...review.findings.map(finding => el('div', { class: 'entry' },
        el('strong', {}, `${finding.severity} · ${finding.path}:${finding.line} (${finding.side}) · ${finding.title}`),
        el('p', {}, finding.body))),
      el('div', { class: 'row' }, files, side,
        el('button', { onclick: () => request({ action: 'source', review: review.id, path: files.value, side: side.value, offset: 0 }) }, 'Read saved source')),
      el('div', { class: 'row' }, line, text, el('button', { onclick: () => request({ action: 'comment', review: review.id,
        comment: { source, path: files.value, side: side.value, line: Number(line.value), text: text.value } }) }, 'Save line comment')),
      ...review.comments.map(comment => el('p', {}, `${comment.path}:${comment.line} (${comment.side}) · ${comment.text}`)),
      el('button', { onclick: () => request({ action: 'feedback', review: review.id, source }) }, 'Put comments into draft'));
  };
  panel.append(el('div', { class: 'row' }, target, revision, el('button', { onclick: () => request({ action: 'capture', selection: {
    target: target.value, paths: paths.value.split('\n').map(path => path.trim()).filter(Boolean), ...(target.value === 'working' ? {} : { revision: revision.value })
  } }) }, 'Capture selected changes')), paths,
    el('div', { class: 'row' }, reviewId, el('button', { onclick: () => request({ action: 'inspect', review: reviewId.value }) }, 'Open review'),
      el('button', { onclick: () => request({ action: 'list' }) }, 'List saved reviews')), content);
  return panel;
}
