// AgentStateGraph Explorer — panel logic
(function () {
  'use strict';

  // Tab switching
  const tabs = document.querySelectorAll('.tab');
  const panels = document.querySelectorAll('.panel');
  tabs.forEach(tab => {
    tab.addEventListener('click', () => {
      tabs.forEach(t => t.classList.remove('active'));
      panels.forEach(p => p.classList.remove('active'));
      tab.classList.add('active');
      document.getElementById('panel-' + tab.dataset.panel).classList.add('active');
    });
  });

  // ─── Dashboard ────────────────────────────────────────
  function renderDashboard() {
    const s = DEMO.stats;
    document.getElementById('dash-commits').textContent = s.commit_count;
    document.getElementById('dash-branches').textContent = s.branch_count;
    document.getElementById('dash-paths').textContent = s.path_count;
    document.getElementById('dash-epochs').textContent = s.epoch_count;
    document.getElementById('dash-agents').innerHTML = s.agents
      .map(a => `<span class="pill pill-agent">${a}</span>`).join('');
    document.getElementById('dash-categories').innerHTML = s.categories
      .map(c => `<span class="pill pill-category">${c}</span>`).join('');
    if (s.latest_commit) {
      document.getElementById('dash-latest').innerHTML =
        `<div class="detail-row"><span class="dr-label">ID</span><span class="dr-value mono">${s.latest_commit.id}</span></div>` +
        `<div class="detail-row"><span class="dr-label">Agent</span><span class="dr-value">${s.latest_commit.agent}</span></div>` +
        `<div class="detail-row"><span class="dr-label">Intent</span><span class="dr-value">${s.latest_commit.intent}</span></div>` +
        `<div class="detail-row"><span class="dr-label">Time</span><span class="dr-value">${fmtTime(s.latest_commit.timestamp)}</span></div>`;
    }
  }

  // ─── State Explorer ───────────────────────────────────
  function renderStateTree() {
    const sidebar = document.getElementById('state-tree');
    // Build tree structure from flat paths
    const tree = {};
    DEMO.paths.forEach(p => {
      const parts = p.split('/').filter(Boolean);
      let node = tree;
      parts.forEach((part, i) => {
        if (!node[part]) node[part] = i === parts.length - 1 ? null : {};
        if (node[part] !== null) node = node[part];
      });
    });

    function renderNode(obj, prefix, depth) {
      let html = '';
      const keys = Object.keys(obj).sort();
      for (const key of keys) {
        const path = prefix + '/' + key;
        const isLeaf = obj[key] === null;
        const indent = '&nbsp;'.repeat(depth * 3);
        const icon = isLeaf ? '◆' : '▸';
        html += `<div class="tree-item" data-path="${path}" onclick="selectPath('${path}')">` +
          `<span class="tree-indent">${indent}</span>` +
          `<span class="tree-icon">${icon}</span>` +
          `<span class="tree-key">${key}</span></div>`;
        if (!isLeaf) {
          html += renderNode(obj[key], path, depth + 1);
        }
      }
      return html;
    }

    sidebar.innerHTML = renderNode(tree, '', 0);
  }

  // Global path selection handler
  window.selectPath = function (path) {
    document.querySelectorAll('.tree-item').forEach(el => el.classList.remove('active'));
    document.querySelector(`.tree-item[data-path="${path}"]`)?.classList.add('active');

    const detail = document.getElementById('state-detail');
    // Resolve value from demo state
    const value = resolvePath(DEMO.state, path);
    const blame = DEMO.blame[path];

    let html = `<div class="detail-card"><h3>${path}</h3>`;
    html += `<pre style="background:var(--bg-primary);padding:10px;border-radius:var(--radius-sm);font-size:12px;overflow-x:auto">${JSON.stringify(value, null, 2)}</pre></div>`;

    if (blame) {
      html += `<div class="detail-card"><h3>Blame</h3>`;
      html += detailRow('Agent', blame.agent_id);
      html += detailRow('Intent', `${blame.intent_category} — ${blame.intent_description}`);
      html += detailRow('Confidence', `<span style="color:var(--success);font-weight:700">${blame.confidence}</span>`);
      html += detailRow('Time', fmtTime(blame.timestamp));
      if (blame.reasoning) html += detailRow('Reasoning', `<em style="color:var(--text-muted)">${blame.reasoning}</em>`);
      if (blame.alternatives) html += detailRow('Alternatives', blame.alternatives);
      if (blame.authority) html += detailRow('Authority', blame.authority);
      html += `</div>`;
    }

    detail.innerHTML = html;
  };

  // ─── Timeline ─────────────────────────────────────────
  function renderTimeline() {
    const el = document.getElementById('timeline-list');
    el.innerHTML = DEMO.commits.map(c => {
      const catClass = c.category.toLowerCase() === 'fix' ? ' fix' :
        c.category.toLowerCase() === 'merge' ? ' merge' : '';
      return `<div class="tl-entry">` +
        `<div class="tl-dot${catClass}"></div>` +
        `<div class="tl-card" onclick="this.classList.toggle('open')">` +
        `<div class="tl-header">` +
        `<span class="tl-cat${catClass}">${c.category}</span>` +
        `<span class="tl-agent">${c.agent}</span>` +
        `<span class="tl-conf">${Math.round((c.confidence || 0) * 100)}%</span>` +
        `<span class="tl-time">${fmtTime(c.timestamp)}</span>` +
        `</div>` +
        `<div class="tl-desc">${c.description}</div>` +
        `<div class="tl-expanded">` +
        detailRow('Commit', `<code>${c.id}</code>`) +
        detailRow('Parents', c.parents.length ? c.parents.map(p => `<code>${p}</code>`).join(', ') : '<em>root</em>') +
        (c.reasoning ? detailRow('Reasoning', `<div class="tl-reasoning">${c.reasoning}</div>`) : '') +
        (c.is_merge ? detailRow('Type', '<span style="color:var(--warning)">Merge commit</span>') : '') +
        `</div></div></div>`;
    }).join('');
  }

  // ─── Search ───────────────────────────────────────────
  window.doSearch = function () {
    const query = document.getElementById('search-query').value.toLowerCase().trim();
    const results = document.getElementById('search-results');
    if (!query) { results.innerHTML = ''; return; }

    // Search paths, values, and commit descriptions
    const matches = [];
    DEMO.paths.forEach(p => {
      const val = resolvePath(DEMO.state, p);
      const valStr = JSON.stringify(val);
      if (p.toLowerCase().includes(query) || valStr.toLowerCase().includes(query)) {
        matches.push({ path: p, value: typeof val === 'object' ? JSON.stringify(val) : String(val) });
      }
    });
    DEMO.commits.forEach(c => {
      if (c.description.toLowerCase().includes(query) || (c.reasoning || '').toLowerCase().includes(query)) {
        matches.push({ path: `commit:${c.id}`, value: c.description });
      }
    });

    results.innerHTML = matches.length === 0
      ? '<div class="empty"><p>No matches found</p></div>'
      : matches.map(m =>
          `<div class="search-result"><div class="sr-path">${m.path}</div><div class="sr-value">${m.value}</div></div>`
        ).join('');
  };

  // ─── Branches ─────────────────────────────────────────
  function renderBranches() {
    const el = document.getElementById('branch-list');
    el.innerHTML = DEMO.branches.map(b =>
      `<div class="branch-item${b.name === 'main' ? ' main' : ''}">` +
      `<div class="bi-dot"></div>` +
      `<div class="bi-name">${b.name}</div>` +
      `<div class="bi-commit">${b.commit}</div></div>`
    ).join('');
  }

  // ─── Epochs ───────────────────────────────────────────
  function renderEpochs() {
    const el = document.getElementById('epoch-list');
    el.innerHTML = DEMO.epochs.map(e => {
      const statusClass = e.status.toLowerCase();
      return `<div class="epoch-card">` +
        `<div class="ec-header">` +
        `<span class="ec-id">${e.id}</span>` +
        `<span class="ec-status ${statusClass}">${e.status}</span>` +
        `</div>` +
        `<div class="ec-desc">${e.description}</div>` +
        `<div class="ec-meta">` +
        `<span>${e.commits} commits</span>` +
        `<span>${e.agents.length} agents</span>` +
        (e.sealed_at ? `<span>Sealed ${fmtTime(e.sealed_at)}</span>` : '<span>In progress</span>') +
        `</div>` +
        `<div class="ec-meta" style="margin-top:4px">${e.agents.map(a => `<span class="pill pill-agent">${a}</span>`).join('')}</div>` +
        (e.seal_hash ? `<div class="ec-hash">seal: ${e.seal_hash}</div>` : '') +
        `</div>`;
    }).join('');
  }

  // ─── Intent Tree ──────────────────────────────────────
  function renderIntentTree() {
    const el = document.getElementById('intent-tree');
    // Build tree from commits (parent chain)
    const commits = [...DEMO.commits].reverse(); // chronological
    function renderCommitNode(commit, depth) {
      const children = commits.filter(c => c.parents[0] === commit.id);
      return `<div class="intent-node${depth === 0 ? ' root' : ''}">` +
        `<div class="in-card">` +
        `<div class="in-header">` +
        `<span class="in-cat">${commit.category}</span>` +
        `<span class="in-agent">${commit.agent}</span>` +
        `<span class="in-conf">${Math.round((commit.confidence || 0) * 100)}%</span>` +
        `</div>` +
        `<div class="in-desc">${commit.description}</div>` +
        `</div>` +
        children.map(c => renderCommitNode(c, depth + 1)).join('') +
        `</div>`;
    }
    const roots = commits.filter(c => c.parents.length === 0);
    el.innerHTML = roots.map(r => renderCommitNode(r, 0)).join('');
  }

  // ─── Helpers ──────────────────────────────────────────
  function fmtTime(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    return d.toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
  }

  function detailRow(label, value) {
    return `<div class="detail-row"><span class="dr-label">${label}</span><span class="dr-value">${value}</span></div>`;
  }

  function resolvePath(obj, path) {
    const parts = path.split('/').filter(Boolean);
    let current = obj;
    for (const part of parts) {
      if (current == null) return undefined;
      current = current[part] ?? current[parseInt(part)];
    }
    return current;
  }

  // ─── Initialize ───────────────────────────────────────
  renderDashboard();
  renderStateTree();
  renderTimeline();
  renderBranches();
  renderEpochs();
  renderIntentTree();
})();
