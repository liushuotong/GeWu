'use strict';
const $ = id => document.getElementById(id);
const sections = ['projects', 'tasks', 'proposals', 'attempts', 'artifacts', 'decisions'];
let csrf, labId, data, section = 'projects', selected, pending, generation = 0;
function node(tag, text, className) {
  const el = document.createElement(tag);
  if (text !== undefined) el.textContent = text;
  if (className) el.className = className;
  return el;
}
function button(text, callback, className) {
  const el = node('button', text, className);
  el.type = 'button'; el.addEventListener('click', callback); return el;
}
async function api(path, body) {
  const options = {credentials: 'same-origin', headers: {}};
  if (body) { options.method = 'POST'; options.headers = {'Content-Type': 'application/json', 'X-GeWu-Lab-CSRF': csrf}; options.body = JSON.stringify(body); }
  const response = await fetch(path, options);
  const value = await response.json().catch(() => ({error: `HTTP ${response.status}`}));
  if (!response.ok) throw new Error(value.error || `HTTP ${response.status}`);
  return value;
}
const path = (...parts) => '/api/labs/' + parts.map(encodeURIComponent).join('/');
const title = item => item.name || item.objective || item.title || item.kind || item.id;
function notify(error) { $('notice').textContent = error instanceof Error ? error.message : error; }
async function loadLabs() {
  const value = await api('/api/labs');
  $('labs').replaceChildren();
  for (const lab of value.labs) {
    const el = button(lab.name, () => selectLab(lab.id).catch(notify));
    el.setAttribute('aria-current', String(lab.id === labId));
    $('labs').append(el);
  }
  if (!value.labs.length) notify('No labs yet. Create one with gewu-lab init, then refresh.');
  return value.labs;
}
async function selectLab(id) {
  labId = id; selected = undefined; data = undefined; $('detail').replaceChildren(node('p', 'Select a record to inspect details.', 'empty'));
  await load();
}
async function load() {
  if (!labId) { const labs = await loadLabs(); if (labs.length) await selectLab(labs[0].id); return; }
  const ticket = ++generation, id = labId;
  $('updated').textContent = 'Loading…';
  const fresh = await api(path(id));
  if (ticket !== generation) return;
  data = fresh;
  $('title').textContent = data.lab.name;
  $('interest').textContent = data.interest?.interest || 'No research interest recorded.';
  $('lab-actions').replaceChildren(node('span', data.lab.paused ? 'Paused' : 'Active', 'badge'), action('lab', data.lab, data.lab.paused ? 'resume' : 'pause'));
  $('counts').replaceChildren();
  const metrics = [['Projects', data.projects.length], ['Tasks', data.tasks.length], ['Awaiting review', data.task_counts.awaiting_review || 0], ['Pending proposals', data.proposals.filter(p => p.status === 'proposed').length]];
  for (const [label, count] of metrics) { const el = node('div', undefined, 'metric'); el.append(node('strong', count), node('span', label)); $('counts').append(el); }
  $('updated').textContent = 'Updated ' + new Date().toLocaleTimeString();
  await loadLabs(); render();
  if (selected) { const item = data[section].find(i => i.id === selected); if (item) inspect(item); }
}
function render() {
  $('tabs').replaceChildren(...sections.map(name => {
    const el = button(`${name[0].toUpperCase() + name.slice(1)} · ${data?.[name].length || 0}`, () => { section = name; selected = undefined; $('search').value = ''; $('detail').replaceChildren(node('p', 'Select a record to inspect details.', 'empty')); render(); });
    el.setAttribute('aria-selected', String(section === name)); return el;
  }));
  $('records').replaceChildren();
  const query = $('search').value.toLowerCase();
  const items = (data?.[section] || []).filter(item => JSON.stringify(item).toLowerCase().includes(query));
  for (const item of items) {
    const el = button('', () => { selected = item.id; render(); inspect(item); }, 'record');
    el.setAttribute('aria-pressed', String(item.id === selected));
    const meta = node('span', undefined, 'record-meta');
    meta.append(node('span', item.status || item.kind || 'record', 'badge'), node('span', item.id));
    el.append(node('span', title(item), 'record-title'), meta); $('records').append(el);
  }
  if (!items.length) $('records').append(node('p', query ? 'No matching records.' : 'No records in this section yet.', 'empty'));
}
function field(label, value, pre = false) {
  if (value === null || value === undefined) return;
  $('detail').append(node('h3', label), node(pre ? 'pre' : 'p', typeof value === 'object' ? JSON.stringify(value, null, 2) : String(value)));
}
function inspect(item) {
  const detail = $('detail'); detail.replaceChildren(node('h2', title(item)));
  for (const [key, value] of Object.entries(item)) field(key.replaceAll('_', ' '), value, ['spec', 'content', 'output', 'evidence', 'uncertainties'].includes(key));
  const actions = node('div', undefined, 'actions');
  if (section === 'projects') {
    if (item.status === 'proposed') actions.append(action('project', item, 'approve'));
    field('Project tasks', data.tasks.filter(t => t.project_id === item.id).map(t => `${t.status} · ${t.objective}`).join('\n') || 'None');
  }
  if (section === 'tasks') {
    if (!['completed', 'cancelled'].includes(item.status)) actions.append(action('task', item, 'priority'), action('task', item, 'cancel'));
    field('Attempts', data.attempts.filter(a => a.task_id === item.id).map(a => `${a.status} · ${a.id}`).join('\n') || 'None');
    field('Artifacts', data.artifacts.filter(a => a.task_id === item.id).map(a => a.title).join('\n') || 'None');
  }
  if (section === 'proposals') {
    field('Proposal decisions', data.proposal_decisions.filter(d => d.proposal_id === item.id), true);
    if (item.status === 'proposed') actions.append(action('proposal', item, 'approve'), action('proposal', item, 'reject'));
  }
  if (section === 'attempts' && item.status === 'submitted') {
    field('Review guidance', 'Worker-reported results are unverified. Inspect output, evidence, uncertainties and artifacts before accepting. Rejection returns the task to ready.');
    field('Artifacts', data.artifacts.filter(a => a.attempt_id === item.id), true);
    actions.append(action('attempt', item, 'accept'), action('attempt', item, 'reject'));
  }
  if (actions.childElementCount) detail.append(actions);
}
function action(kind, item, verb) {
  return button(verb === 'priority' ? 'Set priority' : verb[0].toUpperCase() + verb.slice(1), () => {
    pending = {kind, id: item.id, verb, lab: labId};
    $('decision-title').textContent = `${verb[0].toUpperCase() + verb.slice(1)} ${kind}`;
    $('decision-resource').textContent = title(item);
    $('rationale').value = ''; $('decision-error').textContent = '';
    $('priority-label').hidden = verb !== 'priority';
    $('priority').value = item.priority_override ?? item.priority ?? 0;
    $('decision-dialog').showModal(); $('rationale').focus();
  }, ['reject', 'cancel', 'pause'].includes(verb) ? 'danger' : 'primary');
}
$('decision-form').addEventListener('submit', async event => {
  event.preventDefault(); if (!pending) return;
  const body = {actor: $('actor').value.trim(), rationale: $('rationale').value.trim()};
  if (!body.actor || !body.rationale) { $('decision-error').textContent = 'Actor and rationale cannot be blank.'; return; }
  if (pending.verb === 'priority') {
    body.priority = Number($('priority').value);
    if (!$('priority').value || !Number.isSafeInteger(body.priority)) { $('decision-error').textContent = 'Enter a safe integer priority.'; return; }
  }
  $('confirm').disabled = true;
  try {
    await api(path(pending.lab, pending.kind, pending.id, pending.verb), body);
    $('decision-dialog').close(); pending = undefined; notify('Decision recorded in the audit trail.'); await load();
  } catch (error) { $('decision-error').textContent = error.message; }
  finally { $('confirm').disabled = false; }
});
$('dismiss').addEventListener('click', () => $('decision-dialog').close());
$('search').addEventListener('input', render);
$('refresh').addEventListener('click', () => load().catch(notify));
document.documentElement.dataset.theme = localStorage.getItem('lab-theme') || (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark');
$('theme').addEventListener('click', () => { const theme = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark'; document.documentElement.dataset.theme = theme; localStorage.setItem('lab-theme', theme); });
api('/api/config').then(async config => { csrf = config.csrf_token; await load(); }).catch(notify);
