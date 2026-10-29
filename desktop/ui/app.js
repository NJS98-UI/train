/* =====================================================================
 * TraeWork2API 桌面版前端（纯 HTML/CSS/JS，无框架无构建）
 * 逐项对齐 Android MainActivity.kt：4 个 Tab（概览/账号/模型/记录）
 * + 两个二级页（保活设置 / 外观风格），文案与 toast 与 Android 一致。
 * 后端契约见 desktop/app/src/lib.rs（Tauri 2，withGlobalTauri）。
 * ===================================================================== */
(() => {
  'use strict';

  // ------------------------------------------------------------------
  // 基础设施
  // ------------------------------------------------------------------
  const TAU = () => window.__TAURI__;

  /** 调用后端命令（Tauri 2 参数用 camelCase）。 */
  async function inv(cmd, args) {
    return TAU().core.invoke(cmd, args || {});
  }

  const $ = (id) => document.getElementById(id);

  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined && text !== null) n.textContent = String(text);
    return n;
  }

  // Android 常量（MainActivity companion object）
  const SIXTY_MINUTES_SEC = 3600;
  const REFRESH_MODELS_LABEL = '刷新模型目录';
  const MASKED_KEY = 'sk-••••••••••••••••••••••••••';
  const SG_UNSUPPORTED_NOTE =
    '未开发：登录可通，但聊天不支持多轮上下文与外部工具调用；且免费额度极少（约 $1/月）。建议使用国服。';
  const QODER_INTEGRATIONS_URL = 'https://qoder.com/account/integrations';

  /** 外观风格表（对齐 AppStyle.STYLES，色值在 style.css）。 */
  const STYLES = [
    { id: 'indigo',   label: '靛蓝', note: '经典靛蓝 · 中圆角 · 标准间距', cardRadius: 16 },
    { id: 'graphite', label: '石墨', note: '冷灰中性 · 方正小圆角 · 紧凑间距', cardRadius: 6 },
    { id: 'moss',     label: '苔绿', note: '自然苔绿 · 大圆角 · 舒展间距', cardRadius: 24 },
    { id: 'sunset',   label: '落日', note: '暖橙落日 · 大圆角 · 宽松内边距', cardRadius: 20 },
  ];

  /** 全局状态。 */
  const S = {
    page: 'overview',        // overview | account | models | records
    sub: null,               // null | 'keepalive' | 'style'
    settings: { theme: 'light', colorStyle: 'indigo' },
    overview: null,
    region: null,            // {id,name,isSupported,note}
    backend: {},             // {selection,resolved,label,mixed,traeHas,qoderHas}
    traeAccounts: [],
    qoderAccounts: [],
    credits: null,           // {items:{...}, totals:{trae,qoder}, at?, loading?}
    creditsAt: 0,
    models: { loaded: false, count: 0, items: [] },
    modelsQuery: '',
    modelsLoading: false,
    usage: null,
    logs: [],
    autoCheckin: {},         // {enabled,timeText}
    qoderDaily: {},          // {enabled,timeText,lastResults}
    monitor: {},             // {autoSwitch,threshold,interval}
    keepalive: null,         // [{name,ok,detail}]
    apiKeyVisible: false,
    serviceBusy: null,       // '启动中…' | '停止中…' | null
    checkinBusy: false,
    qoderClaimBusy: false,
    creditsLoading: false,
    lastCheckin: null,       // 最近一次一键签到结果 [{platform,uid,name,ok,msg}]
    autoModelsTried: false,
  };

  let creditNodes = [];      // 账号页里可原地刷新的积分文本节点
  let activeClose = null;    // 当前弹窗关闭函数
  let toastTimer = null;

  // ------------------------------------------------------------------
  // Toast（对齐 Android toast()：底部短提示）
  // ------------------------------------------------------------------
  function toast(msg) {
    const host = $('toastHost');
    host.innerHTML = '';
    host.appendChild(el('div', 'toast', msg));
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { host.innerHTML = ''; }, 2600);
  }

  // ------------------------------------------------------------------
  // 剪贴板
  // ------------------------------------------------------------------
  async function copyText(value, tip) {
    let ok = false;
    try {
      await navigator.clipboard.writeText(value);
      ok = true;
    } catch (_) { /* 走降级 */ }
    if (!ok) {
      const ta = document.createElement('textarea');
      ta.value = value;
      ta.style.position = 'fixed';
      ta.style.opacity = '0';
      document.body.appendChild(ta);
      ta.select();
      try { document.execCommand('copy'); ok = true; } catch (_) { /* 忽略 */ }
      ta.remove();
    }
    if (tip) toast(tip);
    return ok;
  }

  // ------------------------------------------------------------------
  // 弹窗（统一 confirmDialog：标题/正文/自绘按钮，风格 surface + 圆角）
  // ------------------------------------------------------------------
  function closeModal() {
    if (activeClose) activeClose();
  }

  function confirmDialog(opts) {
    const {
      title, message = '', okText = '确定', cancelText = '取消',
      danger = false, build = null, onOk = null,
    } = opts;
    const panel = $('modalPanel');
    panel.innerHTML = '';
    panel.appendChild(el('div', 'modal-title', title));
    if (message) panel.appendChild(el('div', 'modal-message', message));
    const content = el('div', 'modal-content');
    if (build) build(content);
    panel.appendChild(content);

    const close = () => {
      $('modalOverlay').classList.add('hidden');
      panel.innerHTML = '';
      activeClose = null;
      document.removeEventListener('keydown', onKey);
    };
    function onKey(e) { if (e.key === 'Escape') close(); }

    const actions = el('div', 'modal-actions');
    if (cancelText !== null) {
      const c = el('button', 'btn outlined', cancelText);
      c.type = 'button';
      c.onclick = close;
      actions.appendChild(c);
    }
    const ok = el('button', 'btn' + (danger ? ' danger' : ''), okText);
    ok.type = 'button';
    ok.onclick = () => { close(); if (onOk) onOk(); };
    actions.appendChild(ok);
    panel.appendChild(actions);

    activeClose = close;
    $('modalOverlay').classList.remove('hidden');
    document.addEventListener('keydown', onKey);
    return { close };
  }

  /** 账号选择弹窗：固定大小，最多露出 5 行，超出滚动（对齐 showPickerDialog）。 */
  const PICKER_ROW_H = 56;
  const PICKER_VISIBLE_ROWS = 5;
  function showPickerDialog(title, rows, count) {
    confirmDialog({
      title, message: '', okText: '关闭', cancelText: null,
      build(c) {
        const list = el('div', 'picker-list');
        rows.forEach((r) => list.appendChild(r));
        const visible = Math.max(1, Math.min(count, PICKER_VISIBLE_ROWS));
        list.style.maxHeight = (visible * (PICKER_ROW_H + 1) + 2) + 'px';
        list.style.overflowY = 'auto';
        c.appendChild(list);
      },
    });
  }

  // ------------------------------------------------------------------
  // 文案 / 格式化 helpers（对齐 MainActivity 私有函数）
  // ------------------------------------------------------------------
  function p2(v) { return String(v).padStart(2, '0'); }

  /** `HH:mm:ss`（对齐 clockText）。 */
  function clockText(ms) {
    const d = new Date(Number(ms) || Date.now());
    return `${p2(d.getHours())}:${p2(d.getMinutes())}:${p2(d.getSeconds())}`;
  }

  /** 倍率文本：`0` / `1` / `2.5`（对齐 priceText）。 */
  function priceText(v) {
    const n = Number(v);
    return Number.isInteger(n) ? String(n) : String(n);
  }

  /** unix 秒 → "yyyy-MM-dd HH:mm（剩余 x 天/即将过期）"（对齐 formatExp）。 */
  function formatExp(epochSec) {
    if (!epochSec || Number(epochSec) <= 0) return '';
    const d = new Date(Number(epochSec) * 1000);
    const s = `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())} ${p2(d.getHours())}:${p2(d.getMinutes())}`;
    const remainDays = (Number(epochSec) - Date.now() / 1000) / 86400;
    const label = remainDays >= 1 ? `剩 ${remainDays.toFixed(1)} 天` : '即将过期';
    return `${s}（${label}）`;
  }

  function regionLabelOf(meta) {
    if (meta.regionName) return meta.regionName;
    return String(meta.regionId || '').toLowerCase() === 'sg' ? '国际服' : '国服';
  }

  /** 带「未开发」后缀的区域名（对齐 regionLabelFull）。 */
  function regionLabelFull(meta) {
    const base = regionLabelOf(meta);
    return base + (meta.isSupported === false ? '（未开发）' : '');
  }

  function creditItem(platform, uid) {
    const items = (S.credits && S.credits.items) || {};
    return items[platform + ':' + uid] || null;
  }

  /** 单账号积分展示文本（后端已算好 label，缺失显示「—」）。 */
  function creditText(platform, uid) {
    const it = creditItem(platform, uid);
    if (it && it.text) return it.text;
    // 回退：账号列表自带的 credit 字段（trae_accounts / qoder_accounts）
    const acc = (platform === 'trae' ? S.traeAccounts : S.qoderAccounts).find((a) => a.uid === uid);
    if (acc && acc.credit && acc.credit.text) return acc.credit.text;
    return '—';
  }

  function platformTotal(platform) {
    const t = (S.credits && S.credits.totals) || {};
    const v = t[platform];
    return (v === undefined || v === null) ? '—' : v;
  }

  /** 该账号是否已被服务端拒（按 uid 记，两个来源同源）。 */
  function authFailedFor(uid) {
    if (!uid) return false;
    const it = creditItem('trae', uid);
    return !!it && it.error === '需重新登录';
  }

  /** Token 是否需要续期（对齐 tokenRenewLabel）。 */
  function tokenRenewLabel(uid, expiresAt) {
    if (authFailedFor(uid)) return '需重新登录';
    if (!expiresAt || Number(expiresAt) <= 0) return '未知（无到期信息）';
    const remainSec = Number(expiresAt) - Date.now() / 1000;
    if (remainSec <= 0) return '已过期 · 需重新登录';
    if (remainSec <= SIXTY_MINUTES_SEC) return '临近到期 · 将自动续期';
    return '有效';
  }

  /** 积分合计卡片的「更新时间」行。 */
  function creditsUpdatedText() {
    const s = S.credits || {};
    if (s.loading || S.creditsLoading) return '正在查询全部账号…';
    const at = typeof s.at === 'number' ? s.at : S.creditsAt;
    if (!at || at <= 0) return '尚未查询（点右上「刷新积分」）';
    return '更新于 ' + clockText(at);
  }

  function activeTrae() { return S.traeAccounts.find((a) => a.active) || null; }
  function activeQoder() { return S.qoderAccounts.find((a) => a.active) || null; }

  function overviewRunning() {
    const o = S.overview || {};
    return !!(o.running ?? o.serverRunning ?? o.is_running ?? false);
  }
  function overviewPort() {
    const o = S.overview || {};
    const p = o.port ?? o.serverPort ?? 8787;
    return Number(p) || 8787;
  }
  function overviewLan() {
    const o = S.overview || {};
    return o.lan ?? o.lanAddress ?? o.lanIp ?? null;
  }
  function overviewApiKey() {
    const o = S.overview || {};
    return String(o.apiKey ?? o.api_key ?? o.key ?? '');
  }

  function backendLabelOf(selection) {
    return selection === 'trae' ? 'Trae' : selection === 'qoder' ? 'Qoder' : '自动';
  }

  // ------------------------------------------------------------------
  // 组件（对齐 MainActivity 的 View 组装 helper）
  // ------------------------------------------------------------------
  function card(tone, build) {
    const c = el('div', 'card' + (tone ? ' ' + tone : ''));
    if (build) build(c);
    return c;
  }

  function row(build, gap) {
    const r = el('div', 'row' + (gap ? ' gap' + gap : ''));
    if (build) build(r);
    return r;
  }

  function grow(node) { node.classList.add('grow'); return node; }

  function text(value, size, bold, colorName) {
    const cls = ['text'];
    if (size) cls.push('fs-' + size);
    if (bold) cls.push('bold');
    const n = el('span', cls.join(' '), value);
    if (colorName === 'primary') n.style.color = 'var(--primary)';
    if (colorName === 'variant') n.style.color = 'var(--on-surface-variant)';
    if (colorName === 'success') n.style.color = 'var(--success)';
    if (colorName === 'warning') n.style.color = 'var(--warning)';
    if (colorName === 'error') n.style.color = 'var(--error)';
    return n;
  }

  function caption(value) { return el('div', 'caption', value); }

  function section(value) { return el('div', 'section', value); }

  function chipBase(value, tone, dotTone, dotOn) {
    const c = el('div', 'chip ' + tone);
    if (dotTone) {
      const d = el('i', 'dot ' + (dotOn ? 'dot-on' : 'dot-off'));
      d.style.color = 'var(--' + dotTone + ')';
      c.appendChild(d);
    }
    c.appendChild(el('span', null, value));
    return c;
  }
  function statusChip(value, tone, on) { return chipBase(value, tone + '-status', tone, on); }
  function chipOk(v) { return el('div', 'chip ok', v); }
  function chipWarn(v) { return el('div', 'chip warn', v); }
  function chipInfo(v) { return el('div', 'chip info', v); }

  function btn(label, opts) {
    const o = opts || {};
    const b = el('button', 'btn' + (o.outlined ? ' outlined' : '') + (o.danger ? ' danger' : '') + (o.full ? ' full' : ''), label);
    b.type = 'button';
    if (o.onClick) b.onclick = o.onClick;
    if (o.disabled) b.disabled = true;
    return b;
  }

  function linkPill(label, onClick, overContainer) {
    const b = el('button', 'link-pill' + (overContainer ? ' over-container' : ''), label);
    b.type = 'button';
    b.onclick = onClick;
    return b;
  }

  function linkPlain(label, onClick) {
    const b = el('button', 'link-plain', label);
    b.type = 'button';
    b.onclick = onClick;
    return b;
  }

  function labelValue(k, v) {
    const r = el('div', 'label-value');
    r.appendChild(el('div', 'k', k));
    r.appendChild(el('div', 'v', v));
    return r;
  }

  function marginTop(node, px) {
    node.style.marginTop = px + 'px';
    return node;
  }

  function shareBar(value, max) {
    const bar = el('div', 'share-bar');
    const ratio = max <= 0 ? 0 : Math.max(0, Math.min(1, value / max));
    const fill = el('div', 'fill');
    fill.style.width = (ratio * 100) + '%';
    bar.appendChild(fill);
    return bar;
  }

  /** 横向账户选择条：只写「账户选择」（对齐 accountBar）。 */
  function accountBar(onClick) {
    const b = el('button', 'account-bar');
    b.type = 'button';
    b.appendChild(el('span', 'fs-value bold', '账户选择'));
    b.appendChild(el('span', 'caret', '▾'));
    b.onclick = onClick;
    return b;
  }

  // ------------------------------------------------------------------
  // 数据加载
  // ------------------------------------------------------------------
  async function loadSettings() {
    try {
      const s = await inv('settings_get');
      if (s) {
        S.settings.theme = s.theme === 'dark' ? 'dark' : 'light';
        S.settings.colorStyle = STYLES.some((x) => x.id === s.colorStyle) ? s.colorStyle : 'indigo';
      }
    } catch (_) { /* 默认值兜底 */ }
    applyStyle();
  }

  function applyStyle() {
    document.body.dataset.style = S.settings.colorStyle;
    document.body.dataset.mode = S.settings.theme === 'dark' ? 'dark' : 'light';
  }

  async function loadOverview() {
    try { S.overview = await inv('overview'); } catch (_) { S.overview = null; }
  }

  async function loadRegion() {
    try { S.region = await inv('trae_region_get'); } catch (_) { S.region = null; }
  }

  async function loadBackend() {
    try { S.backend = (await inv('backend_get')) || {}; } catch (_) { S.backend = {}; }
  }

  async function loadAccounts() {
    try { S.traeAccounts = (await inv('trae_accounts')) || []; } catch (_) { S.traeAccounts = []; }
    try { S.qoderAccounts = (await inv('qoder_accounts')) || []; } catch (_) { S.qoderAccounts = []; }
  }

  async function loadCredits() {
    try {
      const s = await inv('credits_snapshot');
      if (s && typeof s === 'object') {
        S.credits = s;
        const at = typeof s.at === 'number' ? s.at : 0;
        if (at > 0) S.creditsAt = at;
        else if (!S.creditsAt) {
          const t = s.totals || {};
          if ((t.trae && t.trae !== '—') || (t.qoder && t.qoder !== '—')) S.creditsAt = Date.now();
        }
      }
    } catch (_) { /* 保持旧值 */ }
    refreshCreditNodes();
  }

  async function loadModels() {
    try {
      const m = await inv('models_get');
      if (m && typeof m === 'object') {
        S.models = {
          loaded: !!m.loaded,
          count: Number(m.count || (m.items ? m.items.length : 0)) || 0,
          traeLoaded: !!m.traeLoaded,
          qoderLoaded: !!m.qoderLoaded,
          items: Array.isArray(m.items) ? m.items : [],
        };
      }
    } catch (_) { /* 保持旧值 */ }
  }

  async function loadChecks() {
    try { S.autoCheckin = (await inv('auto_checkin_get')) || {}; } catch (_) { S.autoCheckin = {}; }
    try { S.qoderDaily = (await inv('qoder_daily_get')) || {}; } catch (_) { S.qoderDaily = {}; }
    try { S.monitor = (await inv('credit_monitor_get')) || {}; } catch (_) { S.monitor = {}; }
  }

  async function loadKeepalive() {
    try { S.keepalive = (await inv('keepalive_check')) || []; } catch (_) { S.keepalive = null; }
  }

  async function loadUsage() {
    try { S.usage = await inv('usage_summary'); } catch (_) { S.usage = null; }
  }

  async function loadLogs() {
    try {
      const r = await inv('logs_recent');
      const lines = (r && r.lines) || [];
      S.logs = Array.isArray(lines) ? lines.slice(-600) : [];
    } catch (_) { /* 忽略 */ }
  }

  function refreshCreditNodes() {
    for (const n of creditNodes) n.textContent = n.__compute();
  }

  /** 登记一个可原地刷新的积分文本节点。 */
  function registerCredit(node, compute) {
    node.__compute = compute;
    creditNodes.push(node);
    node.textContent = compute();
  }

  // ------------------------------------------------------------------
  // 页面路由（对齐 render / openKeepAlive / openStylePage / redraw）
  // ------------------------------------------------------------------
  const PAGES = {
    overview: { title: '概览', subtitle: '本地服务运行状态', build: overviewPage },
    account:  { title: '账号', subtitle: '账户登录状态', build: accountPage },
    models:   { title: '模型', subtitle: '本地可用的模型表', build: modelsPage },
    records:  { title: '记录', subtitle: '调用用量与记录', build: recordsPage },
  };

  function setPageMeta(title, subtitle) {
    $('pageTitle').textContent = title;
    $('pageSubtitle').textContent = subtitle;
  }

  function setActiveNav(page) {
    document.querySelectorAll('.nav-item').forEach((n) => {
      n.classList.toggle('active', n.dataset.page === page);
    });
  }

  function render(page) {
    S.page = page;
    S.sub = null;
    S.modelsApply = null;
    $('backAction').classList.add('hidden');
    const p = PAGES[page];
    setPageMeta(p.title, p.subtitle);
    const host = $('content');
    host.innerHTML = '';
    creditNodes = [];
    host.appendChild(p.build());
    setActiveNav(page);
    host.scrollTop = 0;
  }

  function openKeepAlive() {
    S.sub = 'keepalive';
    S.modelsApply = null;
    setPageMeta('保活设置', '让服务在后台长期活着');
    $('backAction').classList.remove('hidden');
    const host = $('content');
    host.innerHTML = '';
    creditNodes = [];
    host.appendChild(keepAlivePage());
    setActiveNav('overview'); // Tab 高亮保持在「概览」
  }

  function openStylePage() {
    S.sub = 'style';
    S.modelsApply = null;
    setPageMeta('外观风格', '配色与形态，随你挑');
    $('backAction').classList.remove('hidden');
    const host = $('content');
    host.innerHTML = '';
    creditNodes = [];
    host.appendChild(stylePage());
    setActiveNav('overview');
  }

  function redraw() {
    if (S.sub === 'keepalive') openKeepAlive();
    else if (S.sub === 'style') openStylePage();
    else render(S.page);
  }

  // ------------------------------------------------------------------
  // 概览页（对齐 overviewPage）
  // ------------------------------------------------------------------
  function overviewPage() {
    const wrap = el('div');
    const running = overviewRunning();
    const busy = S.serviceBusy;

    // 状态胶囊
    wrap.appendChild(busy ? chipInfo(busy) : statusChip(running ? '服务运行中' : '服务已停止', running ? 'success' : 'error', running));

    // 后台常驻卡 + 服务地址/API Key 卡（宽屏两栏）
    const grid = el('div', 'grid-2');
    grid.appendChild(keepAliveCard(running));
    grid.appendChild(addressCard());
    wrap.appendChild(grid);

    // 启停按钮
    const label = busy || (running ? '停止服务' : '启动服务');
    const b = btn(label, {
      full: true,
      outlined: running || !!busy,
      onClick: toggleService,
    });
    if (busy) b.disabled = true;
    wrap.appendChild(b);

    // 当前 Trae 账号卡
    wrap.appendChild(traeCard());
    return wrap;
  }

  /** 后台常驻卡（状态 + 两个入口）。 */
  function keepAliveCard(running) {
    const items = Array.isArray(S.keepalive) ? S.keepalive : [];
    const pending = items.filter((it) => !it.ok).length;
    return card('soft', (c) => {
      c.appendChild(row((r) => {
        r.appendChild(grow(text(running ? '后台常驻中' : '服务未启动', 'card', true)));
        r.appendChild(pending === 0 ? chipOk('保活就绪') : chipWarn(pending + ' 项待开启'));
      }));
      const actions = row((r) => {
        r.classList.add('gap8');
        const go = btn(pending === 0 ? '保活设置' : `去设置保活（${pending} 项）`, { outlined: true, onClick: openKeepAlive });
        grow(go);
        r.appendChild(go);
        const st = btn('外观', { outlined: true, onClick: openStylePage });
        st.style.marginLeft = '8px';
        r.appendChild(st);
      });
      marginTop(actions, 12);
      c.appendChild(actions);
    });
  }

  /** 服务地址 + API Key（强调卡）。 */
  function addressCard() {
    const port = overviewPort();
    const lan = overviewLan();
    const apiKey = overviewApiKey();
    return card('emphasis', (c) => {
      c.appendChild(caption('本机地址'));
      const localUrl = 'http://127.0.0.1:' + port + '/v1';
      c.appendChild(marginTop(row((r) => {
        r.classList.add('gap8');
        const u = text(localUrl, 'body', true, 'primary');
        u.classList.add('mono');
        u.style.userSelect = 'text';
        u.style.wordBreak = 'break-all';
        r.appendChild(grow(u));
        r.appendChild(linkPill('复制', () => copyText(localUrl, '已复制 ' + localUrl), true));
      }), 4));
      if (lan) {
        c.appendChild(marginTop(caption('局域网地址'), 10));
        const lanUrl = 'http://' + lan + ':' + port + '/v1';
        c.appendChild(marginTop(row((r) => {
          r.classList.add('gap8');
          const u = text(lanUrl, 'body', true, 'primary');
          u.classList.add('mono');
          u.style.userSelect = 'text';
          u.style.wordBreak = 'break-all';
          r.appendChild(grow(u));
          r.appendChild(linkPill('复制', () => copyText(lanUrl, '已复制 ' + lanUrl), true));
        }), 4));
      }
      c.appendChild(marginTop(el('div', 'divider'), 12));
      c.appendChild(marginTop(caption('API Key'), 12));
      c.appendChild(marginTop(row((r) => {
        r.classList.add('gap8');
        const v = text(S.apiKeyVisible ? apiKey : MASKED_KEY, 'value');
        v.style.wordBreak = 'break-all';
        if (S.apiKeyVisible) v.style.userSelect = 'text';
        r.appendChild(grow(v));
        r.appendChild(linkPill(S.apiKeyVisible ? '隐藏' : '显示', () => {
          S.apiKeyVisible = !S.apiKeyVisible;
          redraw();
        }, true));
        r.appendChild(linkPill('复制', () => copyText(apiKey, '已复制 API Key'), true));
      }), 4));
    });
  }

  /** 概览页的 Trae 账号卡（对齐 addTraeCard）。 */
  function traeCard() {
    const cred = activeTrae();
    return card(null, (c) => {
      if (!cred) {
        c.appendChild(text('未登录 Trae', 'card', true));
        return;
      }
      const hasToken = cred.hasToken !== undefined ? !!cred.hasToken
        : (Number(cred.expiresAt || 0) > 0 || !!(cred.credit));
      c.appendChild(row((r) => {
        r.appendChild(grow(text(cred.nickname || 'Trae 账号', 'card', true)));
        r.appendChild(hasToken
          ? statusChip('转发 已启用', 'success', true)
          : statusChip('转发 未配置', 'warning', false));
      }));
      if (cred.uid) c.appendChild(marginTop(caption('UID: ' + cred.uid + ' · ' + regionLabelFull(cred)), 6));
      if (cred.isSupported === false) c.appendChild(marginTop(caption(SG_UNSUPPORTED_NOTE), 4));
      if (Number(cred.expiresAt || 0) > 0) c.appendChild(marginTop(caption('Token 到期: ' + formatExp(cred.expiresAt)), 4));
    });
  }

  /** 启停本地服务（幂等，异步动作完成后 toast + 重绘）。 */
  async function toggleService() {
    if (S.serviceBusy) return;
    const stopping = overviewRunning();
    S.serviceBusy = stopping ? '停止中…' : '启动中…';
    redraw();
    try {
      if (stopping) await inv('server_stop');
      else await inv('server_start');
      S.serviceBusy = null;
      toast(stopping ? '服务已停止' : '服务已启动');
    } catch (e) {
      S.serviceBusy = null;
      toast((stopping ? '停止失败：' : '启动失败：') + e);
    }
    await loadOverview();
    await loadKeepalive();
    redraw();
  }

  // ------------------------------------------------------------------
  // 账号页（对齐 accountPage）
  // ------------------------------------------------------------------
  function accountPage() {
    creditNodes = [];
    const wrap = el('div');

    // ---- 积分总计 ----
    wrap.appendChild(card(null, (c) => {
      c.appendChild(row((r) => {
        r.appendChild(grow(text('积分总计', 'card', true)));
        r.appendChild(linkPill('刷新', () => refreshCredits(true, false)));
      }));
      const blocks = row((r) => {
        r.classList.add('gap12');
        r.appendChild(grow(creditBlock('Trae', () => platformTotal('trae'))));
        const q = creditBlock('Qoder', () => platformTotal('qoder'));
        q.style.marginLeft = '12px';
        r.appendChild(grow(q));
      });
      marginTop(blocks, 10);
      c.appendChild(blocks);
      const updated = caption('');
      registerCredit(updated, creditsUpdatedText);
      c.appendChild(marginTop(updated, 8));
    }));

    // ---- TRAE / Qoder 账号（宽屏两栏） ----
    const split = el('div', 'split-2');
    const colT = el('div', 'col');
    colT.appendChild(section('TRAE 账号'));
    buildTraeBlock(colT);
    const colQ = el('div', 'col');
    colQ.appendChild(section('Qoder 账号'));
    buildQoderBlock(colQ);
    split.appendChild(colT);
    split.appendChild(colQ);
    wrap.appendChild(split);

    // ---- 聊天转发后端 ----
    wrap.appendChild(section('聊天转发后端'));
    wrap.appendChild(backendRow());

    // ---- 自动换号 ----
    wrap.appendChild(section('自动换号'));
    wrap.appendChild(marginTop(autoSwitchCard(), 0));

    // ---- 账户导出 ----
    wrap.appendChild(section('账户导出'));
    const exportRow = row((r) => {
      r.classList.add('gap8');
      const e = btn('导出', { outlined: true, onClick: exportAccounts });
      grow(e);
      r.appendChild(e);
      const i = btn('导入', { outlined: true, onClick: importAccounts });
      i.style.marginLeft = '8px';
      grow(i);
      r.appendChild(i);
    });
    marginTop(exportRow, 0);
    wrap.appendChild(exportRow);

    // ---- 一键签到 ----
    wrap.appendChild(section('一键签到'));
    wrap.appendChild(marginTop(checkinCard(), 0));

    return wrap;
  }

  function creditBlock(title, compute) {
    const b = el('div');
    b.appendChild(caption(title));
    const v = el('div', 'metric', '—');
    registerCredit(v, compute);
    b.appendChild(marginTop(v, 2));
    return b;
  }

  function buildTraeBlock(col) {
    const metas = S.traeAccounts;
    const cred = activeTrae();
    if (metas.length === 0) {
      col.appendChild(marginTop(card('soft', (c) => c.appendChild(text('未添加账号', 'card', true))), 0));
    } else {
      // 第一张：当前使用的账号
      col.appendChild(marginTop(card(null, (c) => {
        c.appendChild(row((r) => {
          r.appendChild(grow(text((cred && cred.nickname) || 'TRAE 账号', 'card', true)));
          r.appendChild(chipOk('当前使用'));
        }));
        if (cred) {
          c.appendChild(marginTop(labelValue('区域', regionLabelFull(cred)), 10));
          c.appendChild(marginTop(labelValue('UID', cred.uid || ''), 4));
          c.appendChild(marginTop(labelValue('Token', tokenRenewLabel(cred.uid, cred.expiresAt)), 4));
          const cl = caption('');
          registerCredit(cl, () => '积分：' + creditText('trae', cred.uid));
          c.appendChild(marginTop(cl, 6));
          // 卡上直接签到
          const actionRow = row((r) => {
            r.classList.add('gap8');
            const b = btn(traeCheckinLabel(cred), { outlined: true, onClick: () => traeCheckin(cred.uid) });
            if (S.checkinBusy) b.disabled = true;
            if (traeCheckinLabel(cred) !== '签到') b.classList.add('state-dim');
            grow(b);
            r.appendChild(b);
          });
          marginTop(actionRow, 12);
          c.appendChild(actionRow);
          // 桌面契约：定时签到开关（auto_checkin_get/set）
          const on = !!S.autoCheckin.enabled;
          const tRow = row((r) => {
            r.appendChild(grow(text('定时签到', 'value', true)));
            r.appendChild(linkPlain(on ? '已开启' : '已关闭', toggleAutoCheckin));
            const t = caption(String(S.autoCheckin.timeText || ''));
            t.style.marginLeft = '8px';
            r.appendChild(t);
          });
          marginTop(tRow, 12);
          c.appendChild(tRow);
        }
      }), 0));
      // 账户选择条
      col.appendChild(accountBar(showTraeAccountPicker));
    }
    const btns = row((r) => {
      r.classList.add('gap8');
      const l = btn('登录账号', { onClick: startTraeLogin });
      grow(l);
      r.appendChild(l);
      const p = btn('粘贴回调链接', { outlined: true, onClick: promptPasteCallback });
      p.style.marginLeft = '8px';
      grow(p);
      r.appendChild(p);
    });
    marginTop(btns, 10);
    col.appendChild(btns);
  }

  function traeCheckinLabel(cred) {
    if (S.checkinBusy) return '签到中…';
    if (!cred) return '未登录';
    if (authFailedFor(cred.uid)) return '需重新登录';
    return '签到';
  }

  function buildQoderBlock(col) {
    const metas = S.qoderAccounts;
    const active = activeQoder();
    if (metas.length === 0) {
      col.appendChild(marginTop(card('soft', (c) => c.appendChild(text('未添加账号', 'card', true))), 0));
    } else {
      col.appendChild(marginTop(card(null, (c) => {
        c.appendChild(row((r) => {
          r.appendChild(grow(text((active && active.name) || 'Qoder 账号', 'card', true)));
          r.appendChild(chipOk('当前使用'));
        }));
        if (active) {
          c.appendChild(marginTop(labelValue('区域', qoderModeLabel(active.mode)), 10));
          c.appendChild(marginTop(labelValue('UID', active.uid || ''), 4));
          if (active.email) c.appendChild(marginTop(labelValue('邮箱', active.email), 4));
        }
        c.appendChild(marginTop(caption('积分'), 10));
        const cv = el('div', 'metric', '—');
        registerCredit(cv, qoderRemainingLabel);
        c.appendChild(marginTop(cv, 2));
        if (active) {
          const actionRow = row((r) => {
            r.classList.add('gap8');
            const b = btn(S.qoderClaimBusy ? '领取中…' : '领取', { outlined: true, onClick: () => qoderClaimOne(active.uid) });
            if (S.qoderClaimBusy) b.disabled = true;
            grow(b);
            r.appendChild(b);
          });
          marginTop(actionRow, 12);
          c.appendChild(actionRow);
          // 设备身份（桌面仅展示：设置入口无对应后端命令）
          const captured = !!active.fingerprintSet;
          const isGlobal = qoderModeLabel(active.mode) === '国际版';
          c.appendChild(marginTop(caption(
            (captured ? '设备身份：抓包指纹' : '设备身份：本机派生') + (isGlobal ? '' : '（国内版不发送）')
          ), 12));
        }
      }), 0));
      col.appendChild(accountBar(showQoderAccountPicker));
    }
    const btns = row((r) => {
      r.classList.add('gap8');
      const l = btn('登录 Qoder', { onClick: showQoderLoginGuide });
      grow(l);
      r.appendChild(l);
      const p = btn('回填令牌', { outlined: true, onClick: promptQoderPat });
      p.style.marginLeft = '8px';
      grow(p);
      r.appendChild(p);
    });
    marginTop(btns, 10);
    col.appendChild(btns);
  }

  function qoderModeLabel(mode) {
    return String(mode || '').toLowerCase() === 'cn' ? '国内版' : '国际版';
  }

  /** Qoder 当前账号积分大字（没有读数时显示「—」）。 */
  function qoderRemainingLabel() {
    const uid = activeQoder() && activeQoder().uid;
    if (uid) {
      const it = creditItem('qoder', uid);
      if (it) return it.text || '—';
    }
    return '—';
  }

  /** 聊天转发后端：三个按钮，当前选择实心高亮。 */
  function backendRow() {
    const sel = S.backend.selection || 'auto';
    const r = row((rr) => {
      rr.classList.add('gap8');
      const mk = (label, value, first) => {
        const b = btn(label, { outlined: sel !== value, onClick: () => pickBackend(value) });
        grow(b);
        rr.appendChild(b);
      };
      mk('TRAE', 'trae');
      mk('Qoder', 'qoder');
      mk('自动', 'auto');
    });
    return r;
  }

  async function pickBackend(b) {
    try {
      await inv('backend_set', { b });
      toast('转发后端：' + backendLabelOf(b));
    } catch (e) {
      toast('设置失败：' + e);
    }
    await loadBackend();
    if (S.page === 'account' && !S.sub) render('account');
  }

  /** 自动换号卡（阈值 5/10/15/20；桌面契约另有检测间隔）。 */
  function autoSwitchCard() {
    const auto = !!S.monitor.autoSwitch;
    const cur = Number(S.monitor.threshold || 0);
    const interval = Number(S.monitor.interval || 0);
    return card(null, (c) => {
      c.appendChild(row((r) => {
        r.appendChild(grow(text('当前号用完就换', 'value', true)));
        r.appendChild(btn(auto ? '已开启' : '已关闭', {
          outlined: !auto,
          onClick: async () => {
            try {
              await inv('credit_monitor_set', { autoSwitch: !auto, threshold: cur, interval });
              toast(!auto ? '已开启自动换号' : '已关闭自动换号');
            } catch (e) { toast('设置失败：' + e); }
            await loadChecks();
            if (S.page === 'account' && !S.sub) render('account');
          },
        }));
      }));
      c.appendChild(marginTop(row((r) => {
        r.appendChild(grow(text('换号阈值', 'value', true)));
        r.appendChild(chipInfo('≤ ' + cur));
      }), 12));
      const thRow = row((r) => { r.classList.add('gap8'); });
      [5, 10, 15, 20].forEach((v, i) => {
        const b = btn(String(v), {
          outlined: cur !== v,
          onClick: async () => {
            try { await inv('credit_monitor_set', { autoSwitch: auto, threshold: v, interval }); } catch (e) { toast('设置失败：' + e); }
            await loadChecks();
            if (S.page === 'account' && !S.sub) render('account');
          },
        });
        if (i > 0) b.style.marginLeft = '8px';
        grow(b);
        thRow.appendChild(b);
      });
      marginTop(thRow, 10);
      c.appendChild(thRow);
      c.appendChild(marginTop(caption(
        interval > 0
          ? '只在同一个平台内换号；换完立刻刷新新号的余额。每 ' + interval + ' 分钟自动检测一次。'
          : '只在同一个平台内换号；换完立刻刷新新号的余额。不做定时检测。'
      ), 8));
      // 检测间隔（桌面契约字段）
      const iRow = row((r) => {
        r.classList.add('gap8');
        const lab = caption('检测间隔（分钟，0 = 不定时检测）');
        r.appendChild(lab);
        const input = el('input', 'input');
        input.type = 'number';
        input.min = '0';
        input.value = String(interval);
        input.style.width = '110px';
        input.style.minHeight = '40px';
        input.style.marginLeft = 'auto';
        const apply = btn('应用', {
          outlined: true,
          onClick: async () => {
            const v = Math.max(0, Number(input.value) || 0);
            try { await inv('credit_monitor_set', { autoSwitch: auto, threshold: cur, interval: v }); } catch (e) { toast('设置失败：' + e); }
            await loadChecks();
          },
        });
        r.appendChild(input);
        r.appendChild(apply);
      });
      marginTop(iRow, 8);
      c.appendChild(iRow);
    });
  }

  // ------------------------------------------------------------------
  // 账号选择弹窗（对齐 showTraeAccountPicker / showQoderAccountPicker）
  // ------------------------------------------------------------------
  function pickerRowEl(opts) {
    const { name, meta, credit, active, onClick, onDelete } = opts;
    const r = el('div', 'picker-row');
    r.onclick = onClick;
    r.appendChild(el('div', 'name', name));
    if (meta) r.appendChild(el('div', 'meta', meta));
    if (credit !== undefined && credit !== null) r.appendChild(el('div', 'meta credit', credit));
    if (active) r.appendChild(el('div', 'tick', '✓'));
    const del = el('button', 'del', '删除');
    del.type = 'button';
    del.onclick = (e) => { e.stopPropagation(); onDelete(); };
    r.appendChild(del);
    return r;
  }

  function showTraeAccountPicker() {
    const metas = S.traeAccounts;
    const active = activeTrae();
    if (metas.length === 0) { toast('还没有账号'); return; }
    const rows = metas.map((m) => pickerRowEl({
      name: m.nickname || m.uid,
      meta: regionLabelOf(m),
      credit: '积分：' + creditText('trae', m.uid),
      active: active && m.uid === active.uid,
      onClick: () => {
        closeModal();
        if (!active || m.uid !== active.uid) switchTraeAccount(m);
      },
      onDelete: () => confirmRemoveTrae(m),
    }));
    showPickerDialog('选择 Trae 账号', rows, metas.length);
  }

  function showQoderAccountPicker() {
    const metas = S.qoderAccounts;
    const active = activeQoder();
    if (metas.length === 0) { toast('还没有账号'); return; }
    const rows = metas.map((m) => pickerRowEl({
      name: m.name || m.uid,
      meta: qoderModeLabel(m.mode),
      credit: '积分：' + creditText('qoder', m.uid),
      active: active && m.uid === active.uid,
      onClick: () => {
        closeModal();
        if (!active || m.uid !== active.uid) switchQoderAccount(m);
      },
      onDelete: () => confirmRemoveQoder(m),
    }));
    showPickerDialog('选择 Qoder 账号', rows, metas.length);
  }

  async function afterAccountsChanged() {
    await Promise.all([loadAccounts(), loadCredits(), loadBackend(), loadOverview()]);
    redraw();
  }

  function switchTraeAccount(meta) {
    // 切到未开发区域的账号会让转发走进半残链路，先确认。
    if (meta.isSupported === false) {
      confirmDialog({
        title: '该账号属未开发区域',
        message: (meta.nickname || meta.uid) + '（' + regionLabelOf(meta) + '）\n\n' +
          SG_UNSUPPORTED_NOTE + '\n\n仍然切换吗？',
        okText: '仍要切换',
        cancelText: '取消',
        onOk: () => doSwitchTrae(meta),
      });
      return;
    }
    doSwitchTrae(meta);
  }

  async function doSwitchTrae(meta) {
    try {
      await inv('trae_set_active', { uid: meta.uid });
      toast('已切换到 ' + (meta.nickname || meta.uid));
    } catch (e) {
      toast('切换失败：' + e);
    }
    await afterAccountsChanged();
  }

  function confirmRemoveTrae(meta) {
    confirmDialog({
      title: '删除账号',
      message: '删除「' + (meta.nickname || meta.uid) + '」？本地凭证会一起清掉。',
      okText: '删除',
      danger: true,
      onOk: async () => {
        try {
          await inv('trae_remove', { uid: meta.uid });
          try { await inv('credits_forget', { platform: 'trae', uid: meta.uid }); } catch (_) { /* 忽略 */ }
          toast('已删除');
        } catch (e) { toast('删除失败：' + e); }
        await afterAccountsChanged();
      },
    });
  }

  async function switchQoderAccount(meta) {
    try {
      await inv('qoder_set_active', { uid: meta.uid });
      toast('已切换到 Qoder ' + meta.uid);
    } catch (e) {
      toast('切换失败：' + e);
    }
    await afterAccountsChanged();
  }

  function confirmRemoveQoder(meta) {
    confirmDialog({
      title: '删除账号',
      message: '删除「' + (meta.name || meta.uid) + '」？本地令牌会一起清掉。',
      okText: '删除',
      danger: true,
      onOk: async () => {
        try {
          await inv('qoder_remove', { uid: meta.uid });
          try { await inv('credits_forget', { platform: 'qoder', uid: meta.uid }); } catch (_) { /* 忽略 */ }
          toast('已删除');
        } catch (e) { toast('删除失败：' + e); }
        await afterAccountsChanged();
      },
    });
  }

  // ------------------------------------------------------------------
  // 登录流程：Trae（prepare → 浏览器 → 粘贴回调 → finish）
  // ------------------------------------------------------------------
  async function startTraeLogin() {
    const region = S.region;
    if (region && region.isSupported === false) {
      toast(region.note || (region.id + ' 区域未开放'));
      return;
    }
    let prep = null;
    try { prep = await inv('trae_login_prepare'); } catch (e) { toast('登录失败：' + e); return; }
    const url = (prep && prep.url) || '';
    if (!url) { toast('登录失败：未取得登录链接'); return; }
    await copyText(url, null);
    toast('登录链接已复制');
    try { await inv('open_browser', { url }); } catch (_) { /* 没有可用浏览器，链接已复制 */ }
    promptPasteCallback();
  }

  function promptPasteCallback() {
    const edit = el('textarea', 'textarea');
    edit.placeholder = '粘贴登录后跳转的完整链接';
    edit.rows = 4;
    confirmDialog({
      title: '粘贴回调链接',
      message: '用系统浏览器打开登录链接登录后，把地址栏里那串 http://127.0.0.1:18080/authorize?... 完整粘进来。',
      okText: '登录',
      build(c) { c.appendChild(edit); },
      onOk: async () => {
        const url = edit.value.trim();
        if (!url) { toast('没有粘贴内容，请重新打开本窗口'); return; }
        toast('正在换取 token…');
        try {
          const cred = await inv('trae_login_finish', { callbackUrl: url });
          toast('登录成功：' + ((cred && cred.nickname) || (cred && cred.uid) || 'Trae'));
        } catch (e) {
          toast('登录失败：' + e);
        }
        await afterAccountsChanged();
      },
    });
  }

  // ------------------------------------------------------------------
  // Qoder：登录引导 / 粘贴 PAT
  // ------------------------------------------------------------------
  function showQoderLoginGuide() {
    confirmDialog({
      title: '登录 Qoder',
      message: '1. 登录后点左侧「服务集成」\n2. 在「个人访问令牌」处生成并复制令牌\n3. 回到本页点「回填令牌」粘贴',
      okText: '打开浏览器',
      onOk: async () => {
        try { await inv('open_browser', { url: QODER_INTEGRATIONS_URL }); }
        catch (_) { await copyText(QODER_INTEGRATIONS_URL, '链接已复制，请用浏览器打开'); }
      },
    });
  }

  function promptQoderPat() {
    const edit = el('textarea', 'textarea');
    edit.placeholder = '粘贴 Qoder 个人访问令牌 pt-…';
    edit.rows = 4;
    confirmDialog({
      title: '粘贴 Qoder PAT 登录',
      message: '在 Qoder 账号集成页创建个人访问令牌后粘贴。',
      okText: '登录',
      build(c) { c.appendChild(edit); },
      onOk: async () => {
        const pat = edit.value.trim();
        if (!pat) { toast('没有粘贴内容'); return; }
        toast('正在换取 jobToken…');
        try {
          const c = await inv('qoder_add_pat', { pat });
          toast('登录成功：' + ((c && c.name) || (c && c.email) || (c && c.uid) || 'Qoder'));
        } catch (e) {
          toast('登录失败：' + e);
        }
        await afterAccountsChanged();
      },
    });
  }

  // ------------------------------------------------------------------
  // 签到（Trae 卡片 / Qoder 卡片 / 一键签到）
  // ------------------------------------------------------------------
  function normCheckinResults(res) {
    if (!res) return [];
    if (Array.isArray(res)) return res;
    if (Array.isArray(res.results)) return res.results;
    if (Array.isArray(res.items)) return res.items;
    return [];
  }

  /** Trae 卡上的「签到」：跑一遍并取该账号的结果。 */
  async function traeCheckin(uid) {
    if (S.checkinBusy) return;
    S.checkinBusy = true;
    if (S.page === 'account' && !S.sub) render('account');
    try {
      const res = await inv('checkin_run_all');
      const rs = normCheckinResults(res);
      S.lastCheckin = rs;
      const mine = rs.find((r) => r.platform === 'trae' && r.uid === uid);
      if (mine && mine.ok) toast(mine.msg || '签到成功');
      else toast((mine && mine.msg) || '签到未生效');
    } catch (e) {
      toast('签到失败：' + e);
    }
    S.checkinBusy = false;
    await Promise.all([loadCredits(), loadChecks()]);
    if (S.page === 'account' && !S.sub) render('account');
  }

  /** Qoder 卡上的「领取」：只看该账号结果。 */
  async function qoderClaimOne(uid) {
    if (S.qoderClaimBusy) return;
    S.qoderClaimBusy = true;
    if (S.page === 'account' && !S.sub) render('account');
    try {
      const res = await inv('checkin_run_all');
      const rs = normCheckinResults(res);
      S.lastCheckin = rs;
      const mine = rs.find((r) => r.platform === 'qoder' && r.uid === uid);
      if (mine && mine.ok) toast(mine.msg || '领取成功');
      else toast((mine && mine.msg) || '无可领');
    } catch (e) {
      toast('领取失败：' + e);
    }
    S.qoderClaimBusy = false;
    await Promise.all([loadCredits(), loadChecks()]);
    if (S.page === 'account' && !S.sub) render('account');
  }

  /** 一键签到：所有平台所有账号（对齐 claimAllCheckin 的入口语义）。 */
  async function claimAllCheckin() {
    if (S.checkinBusy) return;
    if (S.traeAccounts.length === 0 && S.qoderAccounts.length === 0) {
      toast('没有可签到的账号');
      return;
    }
    S.checkinBusy = true;
    if (S.page === 'account' && !S.sub) render('account');
    try {
      const res = await inv('checkin_run_all');
      const rs = normCheckinResults(res);
      S.lastCheckin = rs;
      const ok = rs.filter((r) => r.ok).length;
      if (rs.length === 0) toast('没有可签到的账号');
      else if (ok > 0) toast(ok + '/' + rs.length + ' 个账号签到成功');
      else {
        const failed = rs.find((r) => r.msg);
        toast((failed && failed.msg) || ('无可领额度（已领或不符）· 共 ' + rs.length + ' 个账号'));
      }
    } catch (e) {
      toast('签到失败：' + e);
    }
    S.checkinBusy = false;
    await Promise.all([loadCredits(), loadChecks()]);
    if (S.page === 'account' && !S.sub) render('account');
  }

  /** 一键签到卡下面的状态文字：只报结果，不写解释。 */
  function checkinStatusText() {
    const parts = [];
    const manual = S.lastCheckin || [];
    const manT = manual.filter((r) => r.platform === 'trae');
    if (manT.length) parts.push('Trae：' + (manT.some((r) => r.ok) ? '已签到' : '签到未成功'));
    let qrs = manual.filter((r) => r.platform === 'qoder');
    if (qrs.length === 0) {
      const lr = S.qoderDaily && S.qoderDaily.lastResults;
      const arr = Array.isArray(lr) ? lr : (lr && (lr.results || lr.items)) || [];
      qrs = arr.map((x) => ({ ok: !!(x && (x.ok ?? x.status === 'ok')) }));
    }
    if (qrs.length) {
      const ok = qrs.filter((r) => r.ok).length;
      parts.push('Qoder：' + (ok > 0 ? '领取 ' + ok + ' 个' : '无可领'));
    }
    return parts.join('  ·  ');
  }

  /** 一键签到卡（含 Qoder 定时领取开关 + 时间）。 */
  function checkinCard() {
    return card(null, (c) => {
      const statusView = caption(checkinStatusText());
      c.appendChild(btn('一键签到全部账号', { full: true, onClick: claimAllCheckin }));
      c.appendChild(marginTop(statusView, 10));
      c.appendChild(marginTop(el('div', 'divider'), 14));
      const on = !!(S.qoderDaily && S.qoderDaily.enabled);
      const timeText = String((S.qoderDaily && S.qoderDaily.timeText) || '08:00');
      const tRow = row((r) => {
        r.appendChild(grow(text('定时领取', 'value', true)));
        r.appendChild(linkPlain(on ? '已开启' : '已关闭', toggleQoderDaily));
        const t = linkPlain(timeText, promptQoderTime);
        t.style.marginLeft = '8px';
        r.appendChild(t);
      });
      marginTop(tRow, 12);
      c.appendChild(tRow);
    });
  }

  async function toggleQoderDaily() {
    const cur = S.qoderDaily || {};
    const timeText = String(cur.timeText || '08:00');
    try {
      if (cur.enabled) {
        await inv('qoder_daily_set', { enabled: false, timeText });
        toast('已关闭定时领取');
      } else {
        await inv('qoder_daily_set', { enabled: true, timeText });
        toast('已开启，每天 ' + timeText);
      }
    } catch (e) { toast('设置失败：' + e); }
    await loadChecks();
    if (S.page === 'account' && !S.sub) render('account');
  }

  /** 定时领取时间：两个下拉选小时与分钟（不手打）。 */
  function promptQoderTime() {
    const cur = String((S.qoderDaily && S.qoderDaily.timeText) || '08:00').split(':');
    const h0 = Math.min(23, Math.max(0, parseInt(cur[0], 10) || 0));
    const m0 = Math.min(59, Math.max(0, parseInt(cur[1], 10) || 0));
    const hourSel = el('select');
    for (let h = 0; h <= 23; h++) {
      const o = el('option', null, p2(h));
      o.value = String(h);
      if (h === h0) o.selected = true;
      hourSel.appendChild(o);
    }
    const minSel = el('select');
    for (let m = 0; m <= 59; m++) {
      const o = el('option', null, p2(m));
      o.value = String(m);
      if (m === m0) o.selected = true;
      minSel.appendChild(o);
    }
    const panel = el('div', 'time-picker');
    panel.appendChild(hourSel);
    panel.appendChild(el('div', 'colon', ':'));
    panel.appendChild(minSel);
    confirmDialog({
      title: '定时领取时间',
      message: '',
      okText: '确定',
      build(c) { c.appendChild(panel); },
      onOk: async () => {
        const t = p2(Number(hourSel.value)) + ':' + p2(Number(minSel.value));
        try {
          await inv('qoder_daily_set', { enabled: !!(S.qoderDaily && S.qoderDaily.enabled), timeText: t });
          toast('定时领取：每天 ' + t);
        } catch (e) { toast('设置失败：' + e); }
        await loadChecks();
        if (S.page === 'account' && !S.sub) render('account');
      },
    });
  }

  async function toggleAutoCheckin() {
    const on = !!(S.autoCheckin && S.autoCheckin.enabled);
    try {
      await inv('auto_checkin_set', { enabled: !on });
      toast(!on ? '已开启定时签到' : '已关闭定时签到');
    } catch (e) { toast('设置失败：' + e); }
    await loadChecks();
    if (S.page === 'account' && !S.sub) render('account');
  }

  // ------------------------------------------------------------------
  // 积分刷新
  // ------------------------------------------------------------------
  async function refreshCredits(force, silent) {
    if (S.creditsLoading) return;
    S.creditsLoading = true;
    if (!silent) toast('正在查询全部账号积分…');
    refreshCreditNodes();
    try {
      const s = await inv('credits_refresh', { force: !!force });
      if (s && typeof s === 'object' && s.items) {
        S.credits = s;
        const at = typeof s.at === 'number' ? s.at : 0;
        if (at > 0) S.creditsAt = at;
        else if (!S.creditsAt) S.creditsAt = Date.now();
      }
    } catch (_) { /* 保留旧值 */ }
    S.creditsLoading = false;
    refreshCreditNodes();
  }

  // ------------------------------------------------------------------
  // 账户导出 / 导入
  // ------------------------------------------------------------------
  async function exportAccounts() {
    let data = null;
    try { data = await inv('export_accounts'); } catch (e) { toast('导出失败：' + e); return; }
    const trae = data && data.trae != null ? (typeof data.trae === 'string' ? data.trae : JSON.stringify(data.trae, null, 2)) : '';
    const qoder = data && data.qoder != null ? (typeof data.qoder === 'string' ? data.qoder : JSON.stringify(data.qoder, null, 2)) : '';
    const combined = JSON.stringify({ trae: trae, qoder: qoder }, null, 2);
    const edit = el('textarea', 'textarea mono');
    edit.value = combined;
    edit.rows = 10;
    edit.readOnly = true;
    confirmDialog({
      title: '导出账号 JSON',
      message: '以下内容含明文 token，请妥善保管。可整段复制保存为文件。',
      okText: '复制',
      cancelText: '关闭',
      build(c) { c.appendChild(edit); },
      onOk: async () => {
        await copyText(edit.value, '账号 JSON 已复制到剪贴板（含明文 token，请妥善保管）');
      },
    });
  }

  function importAccounts() {
    const edit = el('textarea', 'textarea mono');
    edit.placeholder = '粘贴导出的账号 JSON（支持 TraeWork2API 合并备份或单平台导出）';
    edit.rows = 10;
    confirmDialog({
      title: '导入账号 JSON',
      message: '',
      okText: '导入',
      build(c) { c.appendChild(edit); },
      onOk: async () => {
        const raw = edit.value.trim();
        if (!raw) { toast('没有粘贴内容'); return; }
        let traeStr = '';
        let qoderStr = '';
        try {
          const parsed = JSON.parse(raw);
          if (parsed && (parsed.trae !== undefined || parsed.qoder !== undefined)) {
            traeStr = parsed.trae != null ? (typeof parsed.trae === 'string' ? parsed.trae : JSON.stringify(parsed.trae)) : '';
            qoderStr = parsed.qoder != null ? (typeof parsed.qoder === 'string' ? parsed.qoder : JSON.stringify(parsed.qoder)) : '';
          } else {
            // 单平台导出：按内容特征粗分（Qoder 备份含 jobToken / q_ 前缀键）
            const s = JSON.stringify(parsed);
            if (/"jobToken"|"pt-|q_acc_/.test(s)) qoderStr = s;
            else traeStr = s;
          }
        } catch (_) {
          toast('导入失败：JSON 无效或没有可导入的账号');
          return;
        }
        try {
          const r = await inv('import_accounts', { trae: traeStr, qoder: qoderStr });
          toast('已导入：Trae ' + ((r && r.trae) || 0) + ' 个 · Qoder ' + ((r && r.qoder) || 0) + ' 个');
        } catch (e) {
          toast('导入失败：JSON 无效或没有可导入的账号（' + e + '）');
          return;
        }
        await afterAccountsChanged();
      },
    });
  }

  // ------------------------------------------------------------------
  // 模型页（对齐 modelsPage / modelCard）
  // ------------------------------------------------------------------
  function modelsPage() {
    const wrap = el('div');

    const edit = el('input', 'input');
    edit.type = 'text';
    edit.placeholder = '搜索模型…';
    edit.value = S.modelsQuery;
    wrap.appendChild(edit);

    // 工具条（计数 + 刷新按钮）
    const countView = caption('');
    const refreshBtn = btn(REFRESH_MODELS_LABEL, { onClick: () => refreshModelsFlow(false) });
    const toolbar = row((r) => {
      r.classList.add('gap8');
      r.appendChild(refreshBtn);
      grow(countView);
    });
    marginTop(toolbar, 12);
    wrap.appendChild(toolbar);

    // 未加载空状态
    const notLoaded = card(null, (c) => {
      c.appendChild(text('模型目录尚未加载', 'card', true));
      c.appendChild(marginTop(btn(REFRESH_MODELS_LABEL, { full: true, onClick: () => refreshModelsFlow(false) }), 12));
    });
    wrap.appendChild(notLoaded);

    // 搜不到时的空态
    const empty = card('soft', (c) => {
      c.appendChild(caption('没有匹配的模型。换个关键词，或清空搜索框看看全部。'));
    });
    empty.classList.add('hidden');
    wrap.appendChild(empty);

    // 卡片容器（宽屏多列网格）
    const cardHost = el('div', 'grid-cards');
    wrap.appendChild(cardHost);

    // Qoder CLI 目录导入入口
    wrap.appendChild(marginTop(linkPill('导入官方模型目录', importCatalog), 0));

    const cards = new Map();
    function buildCards() {
      cardHost.innerHTML = '';
      cards.clear();
      for (const m of S.models.items) {
        const c = modelCard(m);
        const id = modelIdOf(m);
        cards.set(id, c);
        cardHost.appendChild(c);
      }
      applyFilter();
    }
    function applyFilter() {
      const f = (S.modelsQuery || '').trim();
      let anyVisible = false;
      cards.forEach((v, key) => {
        const show = !f || key.toLowerCase().includes(f.toLowerCase());
        v.classList.toggle('hidden', !show);
        if (show) anyVisible = true;
      });
      empty.classList.toggle('hidden', !(f && !anyVisible));
    }
    edit.addEventListener('input', () => {
      S.modelsQuery = edit.value;
      applyFilter();
    });

    function applyLoadedState() {
      const loaded = !!S.models.loaded;
      notLoaded.classList.toggle('hidden', loaded);
      edit.classList.toggle('hidden', !loaded);
      toolbar.classList.toggle('hidden', !loaded);
      cardHost.classList.toggle('hidden', !loaded);
      if (!loaded) empty.classList.add('hidden');
    }
    function refreshCountLabel() {
      countView.textContent = '已加载 · ' + S.models.items.length + ' 个模型';
    }
    function refreshButtonLabel() {
      refreshBtn.disabled = S.modelsLoading;
      refreshBtn.textContent = S.modelsLoading ? '刷新中…' : REFRESH_MODELS_LABEL;
    }

    buildCards();
    refreshCountLabel();
    refreshButtonLabel();
    applyLoadedState();

    // 供刷新完成后原地重建（保留搜索框内容）
    S.modelsApply = () => {
      buildCards();
      refreshCountLabel();
      refreshButtonLabel();
      applyLoadedState();
    };
    return wrap;
  }

  function modelIdOf(m) {
    return String(m.id || m.config_name || '');
  }

  function modelCard(m) {
    const id = modelIdOf(m);
    const name = m.name || id;
    const key = m.key || id;
    const hasPf = m.price_factor !== undefined && m.price_factor !== null;
    const pf = hasPf ? Number(m.price_factor) : null;
    const unverified = !!m.unverified;
    const backendTag = m.owned_by || ((S.backend.resolved === 'qoder') ? 'qoder' : 'trae-solo');
    const pfText = pf === null ? '倍率未知' : (pf <= 0 ? '免费（0 倍率）' : '倍率 ×' + priceText(pf));
    const keyNote = unverified ? ' · key 未验证' : '';
    return card(null, (c) => {
      c.appendChild(row((r) => {
        r.appendChild(grow(text(name, 'card', true)));
        if (pf === null) r.appendChild(chipInfo('倍率未知'));
        else if (pf <= 0) r.appendChild(chipOk('免费'));
        else r.appendChild(chipInfo('×' + priceText(pf)));
      }));
      c.appendChild(marginTop(row((r) => {
        r.classList.add('gap4');
        const k = el('div', 'mono fs-chip', key);
        k.style.color = 'var(--on-surface-variant)';
        k.style.overflow = 'hidden';
        k.style.textOverflow = 'ellipsis';
        k.style.whiteSpace = 'nowrap';
        k.style.userSelect = 'text';
        r.appendChild(grow(k));
        r.appendChild(linkPill('复制', () => copyText(id, '已复制模型 ID: ' + id)));
      }), 4));
      c.appendChild(marginTop(caption('context ' + (Number(m.context_length) || 0) + ' · ' + pfText + ' · ' + backendTag + keyNote), 4));
    });
  }

  /** 「刷新模型目录」：静默模式不 toast、不切按钮态（对齐 refreshModels(quiet)）。 */
  async function refreshModelsFlow(quiet) {
    if (S.modelsLoading) return;
    S.modelsLoading = true;
    if (!quiet) {
      if (S.modelsApply) S.modelsApply();
      toast(S.backend.resolved === 'qoder' ? '正在拉取 Qoder 官方模型目录…' : '正在加载模型目录…');
    }
    let res = null;
    try { res = await inv('models_refresh'); } catch (_) { res = null; }
    S.modelsLoading = false;
    if (res && !quiet) {
      const t = res.trae || {};
      const q = res.qoder || {};
      const msgs = [];
      if (S.backend.resolved !== 'qoder') {
        msgs.push(t.msg || (t.ok ? '已更新（' + (t.count || 0) + ' 个模型）' : '拉取失败，请检查网络后重试'));
      }
      if (S.backend.resolved !== 'trae') {
        msgs.push(q.dynamic
          ? '官方目录已同步：' + (q.count || 0) + ' 个模型（' + (q.free || 0) + ' 个免费）'
          : '拉取失败，继续用内置 ' + (q.count || 0) + ' 个模型');
      }
      if (msgs.length) toast(msgs.join('\n'));
    }
    await loadModels();
    if (S.modelsApply) S.modelsApply();
  }

  /** 进 App 静默补新一次模型表（每进程一次，失败无感）。 */
  async function maybeAutoRefreshModels() {
    if (S.autoModelsTried || S.modelsLoading) return;
    S.autoModelsTried = true;
    try { await refreshModelsFlow(true); } catch (_) { /* 静默 */ }
  }

  /** 手动导入 Qoder CLI 的模型目录缓存（对齐 promptImportCatalog）。 */
  async function importCatalog() {
    const active = activeQoder();
    const uid = (active && active.uid) || '';
    let hint = '';
    try {
      const h = await inv('qoder_models_cli_hint', { uid });
      hint = typeof h === 'string' ? h : String((h && (h.hint || h.cmd || h.command)) || '');
    } catch (_) { /* 忽略 */ }
    const edit = el('textarea', 'textarea');
    edit.placeholder = '粘贴 catalog-v7 文件的全部内容（base64），或一段明文目录 JSON';
    edit.rows = 8;
    confirmDialog({
      title: '导入官方模型目录',
      message: '拿到准确的模型 key 和官方倍率（含 0 倍率的免费模型）。',
      okText: '导入',
      build(c) {
        c.appendChild(edit);
        c.appendChild(marginTop(caption('在装过 Qoder CLI 的机器（比如代码空间）上执行：'), 12));
        const pre = el('pre', 'dump-box mono', hint);
        c.appendChild(marginTop(pre, 4));
        c.appendChild(marginTop(caption('把输出的整段内容粘贴到上面即可。解密用当前账号的 uid，你不需要自己解。'), 8));
      },
      onOk: async () => {
        const text = edit.value;
        if (!text.trim()) { toast('没有粘贴内容'); return; }
        let r = null;
        try { r = await inv('qoder_models_import_blob', { text, uid }); } catch (e) { toast(e || '导入失败'); return; }
        if (!r || r.ok === false) { toast((r && (r.error || r.msg)) || '导入失败'); return; }
        await loadModels();
        if (S.modelsApply) S.modelsApply();
        toast('已导入 ' + (r.models || 0) + ' 个模型' + (r.decrypted ? '（已解密）' : '') +
          '，共 ' + S.models.items.length + ' 个');
      },
    });
  }

  // ------------------------------------------------------------------
  // 记录页（消耗汇总 + 调用记录 + 运行日志）
  // ------------------------------------------------------------------
  function normRecord(r) {
    return {
      ts: Number(r.ts ?? r.time ?? Date.now()),
      model: String(r.model ?? ''),
      prompt: Number(r.prompt ?? 0),
      completion: Number(r.completion ?? r.output ?? 0),
      total: Number(r.total ?? 0),
      ms: Number(r.ms ?? r.durationMs ?? r.duration ?? 0),
      ok: r.ok === undefined ? false : !!r.ok,
      err: String(r.err ?? r.errorType ?? r.error ?? ''),
    };
  }

  function parseUsage(u) {
    if (Array.isArray(u)) u = { records: u };
    u = u || {};
    const records = (Array.isArray(u.records) ? u.records : Array.isArray(u.items) ? u.items : Array.isArray(u.all) ? u.all : [])
      .map(normRecord);
    let groups = Array.isArray(u.groups) ? u.groups
      : Array.isArray(u.aggregate) ? u.aggregate
      : Array.isArray(u.aggregates) ? u.aggregates
      : Array.isArray(u.summary) ? u.summary
      : null;
    if (!groups) {
      // 按模型聚合总/输出 token（按首次出现顺序，对齐 UsageStore.aggregate）
      const map = new Map();
      for (const r of records) {
        const g = map.get(r.model) || { model: r.model, total: 0, output: 0 };
        g.total += r.total;
        g.output += r.completion;
        map.set(r.model, g);
      }
      groups = [...map.values()];
    }
    return { records, groups };
  }

  function recordsPage() {
    const wrap = el('div');
    const { records, groups } = parseUsage(S.usage);

    if (records.length === 0) {
      wrap.appendChild(section('消耗汇总'));
      const grid = el('div', 'grid-2');
      grid.appendChild(card('soft', (c) => {
        c.appendChild(caption('暂无记录 —— 发一次聊天请求后这里会显示用量与汇总'));
      }));
      grid.appendChild(card(null, (c) => {
        c.appendChild(text('运行日志', 'card', true));
        const pre = el('pre', 'log-box mono', S.logs.length ? S.logs.join('\n') : '暂无日志');
        pre.id = 'logBox';
        c.appendChild(marginTop(pre, 8));
      }));
      wrap.appendChild(grid);
    } else {
      const grid = el('div', 'grid-2');
      // 消耗汇总卡
      grid.appendChild(card(null, (c) => {
        c.appendChild(text('模型消耗汇总', 'card', true));
        const max = groups.reduce((a, g) => Math.max(a, Number(g.total) || 0), 0);
        for (const g of groups) {
          const r = row((rr) => {
            rr.classList.add('gap8');
            rr.appendChild(grow(text(g.model || '?', 'value', true)));
            const m = el('div', 'mono fs-caption', '总 ' + g.total + ' · 输出 ' + g.output);
            m.style.color = 'var(--on-surface-variant)';
            rr.appendChild(m);
          });
          c.appendChild(marginTop(r, 12));
          c.appendChild(marginTop(shareBar(Number(g.total) || 0, max), 6));
        }
      }));
      // 运行日志卡（桌面新增视图，数据来自 log 事件 + logs_recent）
      grid.appendChild(card(null, (c) => {
        c.appendChild(text('运行日志', 'card', true));
        const pre = el('pre', 'log-box mono', S.logs.length ? S.logs.join('\n') : '暂无日志');
        pre.id = 'logBox';
        c.appendChild(marginTop(pre, 8));
      }));
      wrap.appendChild(grid);

      wrap.appendChild(section('调用记录（最近 ' + records.length + ' 条）'));
      const list = el('div', 'grid-cards');
      for (const r of [...records].reverse()) list.appendChild(recordCard(r));
      wrap.appendChild(list);
    }
    return wrap;
  }

  function recordCard(r) {
    return card(null, (c) => {
      c.appendChild(row((rr) => {
        rr.classList.add('gap8');
        rr.appendChild(statusChip(r.ok ? 'OK' : 'ERR', r.ok ? 'success' : 'error', r.ok));
        rr.appendChild(grow(text(r.model || '?', 'value')));
        const t = el('div', 'mono fs-chip', clockText(r.ts));
        t.style.color = 'var(--on-surface-variant)';
        t.style.marginLeft = '10px';
        rr.appendChild(t);
      }));
      let detail = 'in ' + r.prompt + ' · out ' + r.completion + ' · 总 ' + r.total + ' · ' + r.ms + 'ms';
      if (r.err) detail += ' · ' + r.err;
      c.appendChild(marginTop(caption(detail), 6));
    });
  }

  // ------------------------------------------------------------------
  // 保活设置（二级页；桌面体检项来自 keepalive_check）
  // ------------------------------------------------------------------
  function keepAlivePage() {
    const wrap = el('div');
    const items = Array.isArray(S.keepalive) ? S.keepalive : [];
    const pending = items.filter((it) => !it.ok);
    const allOk = pending.length === 0;

    wrap.appendChild(card('no-margin', (c) => {
      c.appendChild(row((r) => {
        r.appendChild(grow(text(allOk ? '保活已就绪' : '还有 ' + pending.length + ' 项待开启', 'card', true)));
        r.appendChild(allOk ? chipOk('✓ 全部正常') : chipWarn('⚠ 需要处理'));
      }));
      c.appendChild(marginTop(caption(allOk
        ? '网关已能在后台长期运行，切到别的 App、息屏都不会断。'
        : '下面标 ⚠ 的项会影响服务长期运行，建议逐项处理。'), 8));
    }));

    wrap.appendChild(section('体检项'));
    wrap.appendChild(card(null, (c) => {
      if (items.length === 0) {
        c.appendChild(caption('暂无体检结果，点「重新检测」试一次。'));
      }
      items.forEach((it, i) => {
        const item = el('div', 'keep-item');
        if (i > 0) item.style.borderTop = '1px solid var(--outline-variant)';
        if (i > 0) item.style.paddingTop = '12px';
        item.appendChild(row((r) => {
          r.appendChild(el('span', 'keep-mark ' + (it.ok ? 'ok' : 'warn'), it.ok ? '✓' : '⚠'));
          r.appendChild(grow(text(it.name || '-', 'value', true)));
        }));
        if (it.detail) item.appendChild(marginTop(caption(it.detail), 4));
        c.appendChild(item);
      });
      c.appendChild(marginTop(btn('重新检测', {
        full: true,
        outlined: true,
        onClick: async () => {
          await loadKeepalive();
          openKeepAlive();
        },
      }), 12));
    }));
    return wrap;
  }

  // ------------------------------------------------------------------
  // 外观风格（二级页；对齐 stylePage）
  // ------------------------------------------------------------------
  function stylePage() {
    const wrap = el('div');
    wrap.appendChild(section('风格'));
    for (const s of STYLES) wrap.appendChild(styleOption(s));

    wrap.appendChild(section('亮暗模式'));
    wrap.appendChild(card(null, (c) => {
      const modes = [
        { id: 'light', label: '浅色', note: '始终使用浅色' },
        { id: 'dark', label: '深色', note: '始终使用深色' },
      ];
      modes.forEach((m, i) => {
        if (i > 0) c.appendChild(marginTop(el('div', 'divider'), 12));
        c.appendChild(marginTop(modeOption(m), i > 0 ? 12 : 0));
      });
    }));
    wrap.appendChild(marginTop(caption('切换后立即生效，并会记住你的选择；重开 App 仍是这套。'), 12));
    return wrap;
  }

  /** 风格卡：左侧色样 + 名称说明，选中项 = 主色描边 + ✓（对齐 styleOption）。 */
  function styleOption(s) {
    const selected = s.id === S.settings.colorStyle;
    const b = el('button', 'style-option' + (selected ? ' selected' : ''));
    b.type = 'button';
    // 色样：用该风格自己的主色与圆角画的小方块，内部一条 on_primary 短胶囊
    const sw = el('div', 'swatch swatch-' + s.id);
    sw.appendChild(el('i'));
    b.appendChild(sw);
    const tx = el('div', 'so-text');
    tx.appendChild(el('div', 'so-title', s.label));
    tx.appendChild(el('div', 'so-note', s.note));
    b.appendChild(tx);
    if (selected) b.appendChild(chipInfo('✓ 使用中'));
    b.onclick = () => switchStyle(s);
    return b;
  }

  /** 亮暗模式选项：整行可点，右侧圆形单选标记（对齐 modeOption / radioDot）。 */
  function modeOption(m) {
    const selected = m.id === S.settings.theme;
    const b = el('button', 'mode-option' + (selected ? ' selected' : ''));
    b.type = 'button';
    const tx = el('div', 'mo-text');
    tx.appendChild(el('div', 'mo-title', m.label));
    tx.appendChild(el('div', 'mo-note', m.note));
    b.appendChild(tx);
    const dot = el('div', 'radio-dot');
    dot.appendChild(el('i'));
    b.appendChild(dot);
    b.onclick = () => switchMode(m);
    return b;
  }

  async function switchStyle(s) {
    if (s.id === S.settings.colorStyle) return;
    S.settings.colorStyle = s.id;
    applyStyle();
    try { await inv('settings_set', { theme: S.settings.theme, colorStyle: s.id }); } catch (_) { /* 本地已生效 */ }
    toast('已切换为「' + s.label + '」');
    openStylePage();
  }

  async function switchMode(m) {
    if (m.id === S.settings.theme) return;
    S.settings.theme = m.id;
    applyStyle();
    try { await inv('settings_set', { theme: m.id, colorStyle: S.settings.colorStyle }); } catch (_) { /* 本地已生效 */ }
    toast('亮暗模式：' + m.label);
    openStylePage();
  }

  // ------------------------------------------------------------------
  // 事件绑定与启动
  // ------------------------------------------------------------------
  function bindChrome() {
    document.querySelectorAll('.nav-item').forEach((n) => {
      n.addEventListener('click', () => render(n.dataset.page));
    });
    $('backAction').addEventListener('click', () => render('overview'));
    $('modalOverlay').addEventListener('click', (e) => {
      if (e.target === $('modalOverlay') && activeClose) activeClose();
    });
  }

  function bindLogEvent() {
    try {
      TAU().event.listen('log', (e) => {
        const p = e && e.payload;
        const line = typeof p === 'string' ? p : (p && (p.line || p.message)) || String(p);
        S.logs.push(line);
        if (S.logs.length > 600) S.logs.shift();
        const box = $('logBox');
        if (box) {
          box.textContent = S.logs.join('\n');
          box.scrollTop = box.scrollHeight;
        }
      });
    } catch (_) { /* 事件不可用时静默 */ }
  }

  async function init() {
    bindChrome();
    await loadSettings();
    bindLogEvent();
    // 并行拉取首屏数据
    await Promise.all([
      loadOverview(), loadRegion(), loadBackend(), loadAccounts(),
      loadCredits(), loadModels(), loadChecks(), loadKeepalive(),
      loadUsage(), loadLogs(),
    ]);
    render('overview');
    // 进 App 静默补新模型表（不弹 toast，失败无感）
    maybeAutoRefreshModels();
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
