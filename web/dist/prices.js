// Public references are reviewed here; only an explicit action saves rates.
import { $, el, api, state, errorCard } from './lib.js';

const rate = value => value == null ? 'unknown' : String(value);
const rates = values => values == null ? 'unknown' :
  `input ${rate(values.input)}, output ${rate(values.output)}, cache read ${rate(values.cache_read)}, cache write ${rate(values.cache_write)}`;

export async function renderPrices() {
  const view = $('#view'), root = el('div'), status = el('p'), body = el('div');
  const refresh = el('button', {}, 'Refresh public reference');
  root.append(el('h2', {}, 'Model price references'), refresh, status, body);
  view.replaceChildren(root);
  const owns = () => state.tab === 'prices' && view.firstChild === root;
  let busy = true, generation = 0, applyButtons = [];
  refresh.disabled = true;
  const draw = listing => {
    body.replaceChildren(); applyButtons = [];
    status.textContent = `${listing.source_url} · observed ${listing.observed_at == null ? 'unknown' : new Date(listing.observed_at * 1000).toISOString()} · age ${listing.age_secs ?? 'unknown'}s · ${listing.stale ? 'stale' : 'reference'}`;
    for (const notice of listing.notices) body.append(el('p', { class: 'hint' }, notice));
    if (!listing.models.length) body.append(el('p', {}, 'No named model sources. Add one in rook config edit.'));
    for (const model of listing.models) {
      const card = el('section', { class: 'card' },
        el('h3', {}, `${model.source} · ${model.model} · ${model.provider ?? 'unknown identity'}`),
        el('p', {}, `USD / million tokens. Configured: ${rates(model.configured)}`),
        el('p', {}, `Reference: ${rates(model.reference)}`),
        el('p', {}, model.reason),
        el('p', { class: 'hint' }, `Reference context: ${model.reference_context ?? 'unknown'}; reasoning: ${model.reference_reasoning ?? 'unknown'}; tools: ${model.reference_tools ?? 'unknown'} (display only)`));
      if (model.last_application) card.append(el('p', { class: 'hint' }, `Last reference application (rates may have been edited): ${model.last_application}`));
      if (model.review_token) {
        const reviewedGeneration = generation;
        const apply = el('button', { onclick: () => {
          if (!owns() || busy || generation !== reviewedGeneration) return;
          if (!confirm(`Apply missing rates to ${model.source} (${model.provider}/${model.model})?\nReference ${rates(model.reference)} USD / million tokens.\nOnly ${model.apply_fields.join(', ')} will be filled.\nSource ${listing.source_url}, observed ${listing.age_secs}s ago.\nManual values and historical receipts remain as saved.`)) return;
          action('/api/models/prices/apply', { source: model.source, review_token: model.review_token });
        } }, 'Apply missing rates');
        applyButtons.push(apply); card.append(apply);
      }
      body.append(card);
    }
  };
  const action = async (path, request) => {
    if (!owns() || busy) return;
    busy = true; ++generation;
    refresh.disabled = true;
    for (const button of applyButtons) button.disabled = true;
    status.textContent = path.endsWith('refresh') ? 'Refreshing public reference…' : 'Checking review and saving missing rates…';
    try {
      await api(path, request);
      const listing = await api('/api/models/prices');
      if (owns()) draw(listing);
    } catch (error) {
      if (owns()) {
        body.replaceChildren(errorCard(error)); applyButtons = [];
        status.textContent = 'Action failed. Inspect again before applying.';
        refresh.textContent = 'Inspect again';
      }
    } finally {
      busy = false;
      if (owns()) refresh.disabled = false;
    }
  };
  refresh.addEventListener('click', async () => {
    if (refresh.textContent === 'Inspect again') {
      if (!owns() || busy) return;
      busy = true;
      refresh.disabled = true;
      try { const listing = await api('/api/models/prices'); if (owns()) { draw(listing); refresh.textContent = 'Refresh public reference'; } }
      catch (error) { if (owns()) body.replaceChildren(errorCard(error)); }
      finally { busy = false; if (owns()) refresh.disabled = false; }
    } else action('/api/models/prices/refresh', {});
  });
  try {
    const listing = await api('/api/models/prices');
    if (owns()) draw(listing);
  } catch (error) {
    if (owns()) { body.replaceChildren(errorCard(error)); refresh.textContent = 'Inspect again'; }
  } finally { busy = false; if (owns()) refresh.disabled = false; }
}
