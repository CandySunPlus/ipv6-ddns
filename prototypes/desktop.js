// Throwaway UI study: compact native window, settings workspace, and three-step setup.
// No network, storage, credential validation, or operating-system operations.
const platformStates = new Map();
const providers = { cloudflare: 'Cloudflare', aliyun: '阿里云 DNS' };
const escape = (value) => String(value ?? '').replace(/[&<>"']/g, (char) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]);
const draft = () => ({ domain: 'example.com', token: '', accessKeyId: '', accessKeySecret: '', mode: 'stable', suffix: '8023', automatic: true });

function createState(scenario) {
  const configured = scenario !== 'unconfigured';
  const initial = draft();
  return {
    scenario, provider: 'aliyun', activeProvider: configured ? 'aliyun' : null,
    drafts: { cloudflare: initial, aliyun: draft() },
    saved: { cloudflare: null, aliyun: configured ? { ...initial, accessKeyId: 'DEMO_ONLY_RAM_ID', accessKeySecret: 'DEMO_ONLY_NOT_A_REAL_SECRET' } : null },
    phase: scenario, installed: configured, running: configured && scenario !== 'stopped',
    expanded: true, section: 'dns', step: 1, completed: false,
    hidden: false, maximized: false, trayOpen: false, notice: '', noticeError: false, lastRun: false,
  };
}

function isDirty(state) {
  const current = state.drafts[state.provider];
  const saved = state.saved[state.provider];
  if (!saved || state.provider !== state.activeProvider) return true;
  return current.domain !== saved.domain || current.mode !== saved.mode || current.suffix !== saved.suffix || current.automatic !== saved.automatic ||
    (state.provider === 'cloudflare' ? !!current.token : !!current.accessKeyId || !!current.accessKeySecret);
}

function status(state) {
  if (!state.running) return { tone: 'muted', title: state.installed ? '后台服务已停止 · 模拟' : '后台服务未安装 · 模拟', detail: '窗口与后台服务相互独立。安装并启动服务后，才会定时同步。' };
  const states = {
    healthy: { tone: 'success', title: '记录已同步 · 模拟', detail: 'AAAA 记录与本机稳定公网 IPv6 一致。此处为预设演示结果。' },
    unconfigured: { tone: 'muted', title: '等待配置 · 模拟', detail: '后台服务等待设置。保存只更新模拟配置，不会立即同步。' },
    'auth-error': { tone: 'error', title: 'DNS 权限错误 · 模拟', detail: '预设错误：凭据无 DNS 写入权限。请检查权限；演示不验证凭据。' },
    'no-ipv6': { tone: 'warning', title: '未找到可用 IPv6 · 模拟', detail: '预设错误：没有符合筛选条件的公网 IPv6，未更新 AAAA 记录。' },
    idle: { tone: 'muted', title: '配置已保存，等待运行 · 模拟', detail: '尚未同步。点击「模拟立即同步」查看演示结果。' },
  };
  return states[state.phase] || states.idle;
}

/** Mount a no-dependency, in-memory macOS or Windows UI prototype. */
export function mountDesktop(root, options = {}) {
  const platform = options.platform === 'win' ? 'win' : 'mac';
  const variant = ['A', 'B', 'C'].includes(options.variant) ? options.variant : 'A';
  const scenario = options.scenario || 'healthy';
  if (!platformStates.has(platform) || platformStates.get(platform).scenario !== scenario) platformStates.set(platform, createState(scenario));
  const state = platformStates.get(platform);
  const current = () => state.drafts[state.provider];
  const button = (action, label, className = '', attributes = '') => `<button type="button" data-action="${action}" class="${className}" ${attributes}>${label}</button>`;

  function emit() {
    const form = current();
    const saved = state.saved[state.provider];
    options.onState?.({
      platform, variant, scenario, selectedProvider: state.provider, activeProvider: state.activeProvider,
      domain: form.domain, recordType: 'AAAA', ipv6Mode: form.mode, suffix: form.mode === 'suffix' ? form.suffix : null,
      automaticStartup: form.automatic, unsaved: isDirty(state), windowHidden: state.hidden,
      service: { simulated: true, installed: state.installed, running: state.running, phase: state.phase, lastRunSimulated: state.lastRun },
      credentials: {
        cloudflare: { draftTokenPresent: !!state.drafts.cloudflare.token, savedTokenPresent: !!state.saved.cloudflare?.token },
        aliyun: { draftIdPresent: !!state.drafts.aliyun.accessKeyId, draftSecretPresent: !!state.drafts.aliyun.accessKeySecret, savedIdPresent: !!state.saved.aliyun?.accessKeyId, savedSecretPresent: !!state.saved.aliyun?.accessKeySecret },
      },
      selectedProviderSaved: !!saved, wizardStep: state.step, wizardCompleted: state.completed,
    });
  }

  function summary() {
    const info = status(state);
    const saved = state.saved[state.activeProvider];
    return `<section class="desktop-summary" aria-label="后台服务状态">
      <span class="desktop-status-icon desktop-tone-${info.tone}" aria-hidden="true">${info.tone === 'success' ? '✓' : info.tone === 'error' ? '!' : info.tone === 'warning' ? '!' : '·'}</span>
      <div><h2>${info.title}</h2><p>${escape(info.detail)}</p>
      <p class="desktop-record">${saved ? `${escape(saved.domain)} <span>AAAA · ${providers[state.activeProvider]}</span>` : '尚未保存域名 · 单条 AAAA 记录'}</p>
      ${saved ? `<code>${state.phase === 'no-ipv6' ? '暂无可用地址' : '2001:db8::8023'}</code>` : ''}
      <p class="desktop-timing">${state.lastRun ? '最近运行：刚刚（模拟） · ' : ''}${!state.running ? '定时检查已停止' : saved?.automatic === false ? '自动更新已关闭 · 可手动同步（模拟）' : state.phase === 'auth-error' || state.phase === 'no-ipv6' ? '失败后每 1 分钟重试（模拟）' : '每 5 分钟检查一次（模拟）'}</p></div>
    </section>`;
  }

  function providerFields() {
    const form = current();
    const saved = state.saved[state.provider];
    return `<div class="desktop-fields">
      <label>DNS 服务商<select name="provider" data-field="provider"><option value="cloudflare" ${state.provider === 'cloudflare' ? 'selected' : ''}>Cloudflare</option><option value="aliyun" ${state.provider === 'aliyun' ? 'selected' : ''}>阿里云 DNS</option></select></label>
      <p class="desktop-note">更换服务商须填写该服务商的凭据；不会复用另一服务商的凭据。各服务商草稿仅在本页内存中保留。</p>
      <label>域名<input name="domain" data-field="domain" value="${escape(form.domain)}" placeholder="example.com 或 home.example.com" autocomplete="off" spellcheck="false"></label>
      ${state.provider === 'cloudflare' ? `<label>Cloudflare API Token<input type="password" name="token" data-field="token" value="${escape(form.token)}" placeholder="${saved?.token ? '已保存本服务商凭据；留空保持不变' : '仅输入虚构 Token'}" autocomplete="off"></label>
      <p class="desktop-note">Cloudflare Token 权限：目标 Zone 的 Zone → Read 与 DNS → Edit。</p>` : `<label>AccessKey ID<input type="password" name="accessKeyId" data-field="accessKeyId" value="${escape(form.accessKeyId)}" placeholder="${saved?.accessKeyId ? '已保存本服务商 ID；留空保持不变' : '仅输入虚构 AccessKey ID'}" autocomplete="off"></label>
      <label>AccessKey Secret<input type="password" name="accessKeySecret" data-field="accessKeySecret" value="${escape(form.accessKeySecret)}" placeholder="${saved?.accessKeySecret ? '已保存本服务商 Secret；留空保持不变' : '仅输入虚构 AccessKey Secret'}" autocomplete="off"></label>
      <p class="desktop-note">阿里云建议使用具有 DNS 访问权限的 RAM 子账号，不使用主账号 AccessKey。</p>`}
      <p class="desktop-secret-warning">交互原型：请勿输入真实密钥。未连接任何 DNS 服务，凭据不会被验证或写入磁盘。</p>
    </div>`;
  }

  function ipv6Fields(includeAutomatic = true) {
    const form = current();
    return `<fieldset class="desktop-ipv6"><legend>IPv6 地址选择</legend>
      <label class="desktop-choice"><input type="radio" name="ipv6-mode" data-field="mode" value="stable" ${form.mode === 'stable' ? 'checked' : ''}>使用稳定公网 IPv6 <span>跳过临时地址</span></label>
      <label class="desktop-choice"><input type="radio" name="ipv6-mode" data-field="mode" value="suffix" ${form.mode === 'suffix' ? 'checked' : ''}>按 IPv6 后缀筛选</label>
      <label class="desktop-suffix" ${form.mode === 'stable' ? 'hidden' : ''}>IPv6 后缀<input name="suffix" data-field="suffix" value="${escape(form.suffix)}" placeholder="例如 8023" spellcheck="false" ${form.mode === 'stable' ? 'disabled' : ''}></label>
      <p class="desktop-note">仅管理当前域名的一条 AAAA 记录。</p></fieldset>
      ${includeAutomatic ? automaticField() : ''}`;
  }

  function automaticField() {
    return `<label class="desktop-choice desktop-auto"><input type="checkbox" name="automatic" data-field="automatic" ${current().automatic ? 'checked' : ''}>开机自动更新</label><p class="desktop-note">${platform === 'mac' ? '系统后台服务' : 'Windows 系统服务'}独立运行，无需登录；关闭窗口不会停止服务。取消此项不会立即停止当前服务，需保存后生效（模拟）。</p>`;
  }

  function saveFooter() {
    return `<div class="desktop-save-row"><span class="desktop-dirty" data-dirty>${isDirty(state) ? '有未保存的更改' : '当前服务商设置已保存'}</span>${button('save', '模拟保存设置', 'desktop-primary')}</div><p class="desktop-note desktop-save-note">保存不触发同步；「模拟立即同步」只使用已保存的配置。</p>`;
  }

  function runButton() {
    return button(state.running ? 'run' : 'install', state.running ? '模拟立即同步' : state.installed ? '模拟启动服务' : '模拟安装并启动服务', 'desktop-primary');
  }

  function servicePanel() {
    return `<section class="desktop-service"><h3>独立后台服务</h3><dl><div><dt>安装状态</dt><dd>${state.installed ? '已安装（模拟）' : '未安装（模拟）'}</dd></div><div><dt>运行状态</dt><dd>${state.running ? '运行中（模拟）' : '已停止（模拟）'}</dd></div><div><dt>检查间隔</dt><dd>每 5 分钟；失败每 1 分钟重试</dd></div></dl>${automaticField()}<p class="desktop-note">服务与设置窗口分离。关闭窗口仅收起到${platform === 'mac' ? '菜单栏' : '托盘'}，后台任务不受影响。</p><div class="desktop-actions">${runButton()}${state.running ? button('stop', '模拟停止服务') : ''}</div></section>`;
  }

  function variantA() {
    return `${summary()}<div class="desktop-compact-actions">${button('expand', state.expanded ? '收起设置' : '展开设置', '', `aria-expanded="${state.expanded}"`)}${runButton()}</div>
      ${state.expanded ? `<section class="desktop-settings"><h3>同步设置 <span>拟议界面</span></h3>${providerFields()}${ipv6Fields()}${saveFooter()}</section>` : ''}`;
  }

  function variantB() {
    const sections = [['overview', '同步概览'], ['dns', 'DNS 与凭据'], ['ipv6', 'IPv6 选择'], ['service', '后台服务']];
    let body;
    if (state.section === 'overview') body = `<h2 class="desktop-page-title">同步概览</h2>${summary()}<div class="desktop-overview-info"><h3>一条记录，一个后台任务</h3><p>修改配置后先保存，再手动模拟同步。检查和重试由独立系统服务承担。</p><p class="desktop-note">所有状态均为预设演示，没有真实 DNS 请求或服务安装。</p></div><div class="desktop-actions">${runButton()}${button('go-dns', '编辑 DNS 设置')}</div>`;
    else if (state.section === 'dns') body = `<h2 class="desktop-page-title">DNS 与凭据</h2>${providerFields()}`;
    else if (state.section === 'ipv6') body = `<h2 class="desktop-page-title">IPv6 选择</h2>${ipv6Fields(false)}`;
    else body = servicePanel();
    return `<div class="desktop-workspace"><nav class="desktop-sidebar" aria-label="设置分类"><strong>IPv6 DDNS</strong>${sections.map(([key, label]) => button(`section-${key}`, label, state.section === key ? 'is-selected' : '', `aria-current="${state.section === key ? 'page' : 'false'}"`)).join('')}<small>系统服务 · 模拟<br>${state.running ? '后台运行中' : '未运行'}</small></nav><section class="desktop-workspace-content">${body}<div class="desktop-workspace-save">${saveFooter()}</div></section></div>`;
  }

  function review() {
    const form = current();
    const saved = state.saved[state.provider];
    const hasCredentials = state.provider === 'cloudflare' ? !!(form.token || saved?.token) : !!((form.accessKeyId || saved?.accessKeyId) && (form.accessKeySecret || saved?.accessKeySecret));
    return `<dl class="desktop-review"><div><dt>DNS 服务商</dt><dd>${providers[state.provider]}</dd></div><div><dt>单条记录</dt><dd>${escape(form.domain || '尚未填写')} · AAAA</dd></div><div><dt>凭据</dt><dd>${hasCredentials ? '已填写或沿用本服务商已保存凭据（未验证）' : '尚未填写'}</dd></div><div><dt>IPv6 策略</dt><dd>${form.mode === 'stable' ? '稳定公网 IPv6' : `后缀 ${escape(form.suffix)}`}</dd></div><div><dt>开机自动更新</dt><dd>${form.automatic ? '开启（模拟）' : '关闭（模拟）'}</dd></div></dl>`;
  }

  function variantC() {
    if (state.completed) return `<section class="desktop-wizard desktop-wizard-done"><div class="desktop-wizard-kicker">设置已保存 · 模拟</div><h2>后台服务独立工作</h2><p class="desktop-note">保存并不代表已同步，也不代表凭据通过验证。</p>${summary()}${review()}<div class="desktop-actions">${button('edit-wizard', '修改设置')}${runButton()}</div><p class="desktop-note">关闭窗口只收起界面，不停止后台服务。</p></section>`;
    return `<section class="desktop-wizard"><ol class="desktop-steps" aria-label="配置步骤">${['DNS 与凭据', 'IPv6 与服务', '检查并保存'].map((label, index) => `<li class="${state.step === index + 1 ? 'is-current' : state.step > index + 1 ? 'is-complete' : ''}" ${state.step === index + 1 ? 'aria-current="step"' : ''}><span>${index + 1}</span>${label}</li>`).join('')}</ol>
      <div class="desktop-wizard-kicker">第 ${state.step} / 3 步 · 拟议配置流程</div><h2>${['连接哪一个 DNS 服务商？', '选择地址与后台行为', '确认设置，只保存不立即同步'][state.step - 1]}</h2>
      ${state.step === 1 ? providerFields() : state.step === 2 ? ipv6Fields() : `${review()}<p class="desktop-secret-warning">原型只修改内存，不安装服务，不验证凭据，不写入 DNS。</p>`}
      <div class="desktop-wizard-actions"><span class="desktop-dirty" data-dirty>${isDirty(state) ? '有未保存的更改' : '当前设置已保存'}</span>${state.step > 1 ? button('previous', '上一步') : ''}${state.step < 3 ? button('next', '下一步', 'desktop-primary') : button('finish', '模拟保存并完成', 'desktop-primary')}</div>
      <div class="desktop-wizard-status">${summary()}<div class="desktop-actions">${runButton()}</div></div></section>`;
  }

  function render() {
    const info = status(state);
    root.innerHTML = `<div class="desktop desktop--${platform} desktop-variant-${variant}">
      ${platform === 'mac' ? `<div class="desktop-menubar"><strong>IPv6 DDNS</strong><span>菜单栏预览</span>${button('tray', `◈ ${state.running ? '后台运行中' : '服务未运行'}`, '', `aria-expanded="${state.trayOpen}"`)}</div>` : ''}
      <div class="desktop-stage">
      ${state.trayOpen ? `<div class="desktop-tray-menu" role="group" aria-label="${platform === 'mac' ? '菜单栏' : '托盘'}菜单"><strong>IPv6 DDNS · 模拟</strong><p>${escape(info.title)}</p>${button('reopen', '打开设置窗口')}${button('tray', '收起菜单')}</div>` : ''}
      ${state.hidden ? `<section class="desktop-hidden"><span class="desktop-hidden-symbol">◈</span><h2>窗口已收起到${platform === 'mac' ? '菜单栏' : '托盘'}</h2><p>${state.running ? '后台服务仍在运行（模拟）。' : '后台服务仍保持停止（模拟）。'}关闭窗口没有改变服务状态。</p>${button('reopen', '重新打开设置', 'desktop-primary')}<p class="desktop-note">${escape(info.title)}</p></section>` : `<section class="desktop-window ${state.maximized ? 'desktop-window-expanded' : ''}" aria-label="${platform === 'mac' ? 'macOS' : 'Windows'} IPv6 DDNS 拟议窗口">
        <header class="desktop-titlebar">${platform === 'mac' ? `<div class="desktop-traffic">${button('close', '<span></span>', 'desktop-traffic-close', 'aria-label="关闭窗口，后台服务继续运行" title="收起到菜单栏"')}${button('minimize', '<span></span>', 'desktop-traffic-minimize', 'aria-label="最小化窗口"')}${button('maximize', '<span></span>', 'desktop-traffic-maximize', 'aria-label="切换窗口大小"')}</div><span>IPv6 DDNS</span><span class="desktop-chrome-spacer"></span>` : `<span class="desktop-app-icon">◈</span><span>IPv6 DDNS</span><div class="desktop-title-controls">${button('minimize', '─', '', 'aria-label="最小化窗口"')}${button('maximize', '□', '', 'aria-label="切换窗口大小"')}${button('close', '×', 'desktop-win-close', 'aria-label="关闭窗口，后台服务继续运行" title="收起到托盘"')}</div>`}</header>
        <div class="desktop-notice ${state.noticeError ? 'desktop-notice-error' : ''}" role="status" ${!state.notice ? 'hidden' : ''}>${escape(state.notice)}</div>
        <main class="desktop-content">${variant === 'A' ? variantA() : variant === 'B' ? variantB() : variantC()}</main>
        <footer class="desktop-window-footnote">拟议界面 · 所有操作均为模拟 · 关闭窗口不停止后台服务</footer>
      </section>`}
      </div>
      ${platform === 'win' ? `<div class="desktop-taskbar"><span>Windows 托盘预览</span>${button('reopen', '◈ IPv6 DDNS', 'desktop-taskbar-app')}${button('tray', `⌃ 托盘 · ${state.running ? '运行中' : '已停止'}`, '', `aria-expanded="${state.trayOpen}"`)}</div>` : ''}
    </div>`;
    emit();
  }

  function notify(message, error = false) {
    state.notice = message;
    state.noticeError = error;
  }

  function save() {
    const form = current();
    const old = state.saved[state.provider];
    if (!form.domain.trim()) { notify('模拟保存未完成：请填写根域名或子域名。', true); return false; }
    if (state.provider === 'cloudflare' && !(form.token.trim() || old?.token)) { notify('模拟保存未完成：请为 Cloudflare 填写虚构 Token，不能沿用阿里云凭据。', true); return false; }
    if (state.provider === 'aliyun' && (!(form.accessKeyId.trim() || old?.accessKeyId) || !(form.accessKeySecret.trim() || old?.accessKeySecret))) { notify('模拟保存未完成：请填写阿里云的虚构 AccessKey ID 和 Secret，不能沿用 Cloudflare Token。', true); return false; }
    if (form.mode === 'suffix' && !form.suffix.trim()) { notify('模拟保存未完成：按后缀筛选时请填写 IPv6 后缀。', true); return false; }
    if (isDirty(state) && state.phase === 'healthy') state.phase = 'idle';
    state.saved[state.provider] = { ...form, domain: form.domain.trim(), token: form.token || old?.token || '', accessKeyId: form.accessKeyId || old?.accessKeyId || '', accessKeySecret: form.accessKeySecret || old?.accessKeySecret || '' };
    state.activeProvider = state.provider;
    form.domain = form.domain.trim();
    form.token = ''; form.accessKeyId = ''; form.accessKeySecret = '';
    if (state.phase === 'unconfigured') state.phase = 'idle';
    notify('已模拟保存所选服务商设置；未同步、未验证凭据，未操作真实系统服务。');
    return true;
  }

  function onClick(event) {
    const target = event.target.closest('[data-action]');
    if (!target || !root.contains(target)) return;
    const action = target.dataset.action;
    if (action === 'close' || action === 'minimize') { state.hidden = true; state.trayOpen = false; }
    else if (action === 'reopen') { state.hidden = false; state.trayOpen = false; }
    else if (action === 'tray') state.trayOpen = !state.trayOpen;
    else if (action === 'maximize') state.maximized = !state.maximized;
    else if (action === 'expand') state.expanded = !state.expanded;
    else if (action.startsWith('section-')) state.section = action.slice(8);
    else if (action === 'go-dns') state.section = 'dns';
    else if (action === 'previous') state.step = Math.max(1, state.step - 1);
    else if (action === 'next') state.step = Math.min(3, state.step + 1);
    else if (action === 'edit-wizard') { state.completed = false; state.step = 1; }
    else if (action === 'save' || action === 'finish') { if (save() && action === 'finish') state.completed = true; }
    else if (action === 'install') {
      state.installed = true; state.running = true;
      if (state.phase === 'stopped') state.phase = state.activeProvider ? 'idle' : 'unconfigured';
      notify('已模拟安装并启动独立后台服务；未立即同步，不涉及真实安装或系统授权。');
    } else if (action === 'stop') {
      state.running = false;
      notify('已明确模拟停止后台服务。此操作与关闭窗口不同。');
    } else if (action === 'run') {
      if (!state.running) notify('模拟同步未运行：请先启动后台服务。', true);
      else if (!state.activeProvider) notify('模拟同步未运行：请先保存服务商、域名与虚构凭据。', true);
      else {
        state.lastRun = true;
        if (state.phase === 'auth-error') notify('模拟重试：预设的 DNS 权限错误仍存在；未验证凭据、未发送请求。', true);
        else if (state.phase === 'no-ipv6') notify('模拟重试：预设的 IPv6 不可用状态仍存在；未更新 DNS。', true);
        else { state.phase = 'healthy'; notify(`模拟同步完成：使用已保存的 ${providers[state.activeProvider]} 配置。仅展示预设成功结果，未验证凭据或更新 DNS。${isDirty(state) ? ' 未保存草稿没有参与本次模拟。' : ''}`); }
      }
    }
    render();
  }

  function onInput(event) {
    const target = event.target;
    const field = target.dataset.field;
    if (!field || field === 'provider') return;
    if (target.type === 'radio' && !target.checked) return;
    if (!Object.hasOwn(current(), field)) return;
    current()[field] = target.type === 'checkbox' ? target.checked : target.value;
    if (field === 'mode' || field === 'automatic') render();
    else {
      root.querySelectorAll('[data-dirty]').forEach((element) => { element.textContent = isDirty(state) ? '有未保存的更改' : '当前服务商设置已保存'; });
      emit();
    }
  }

  function onChange(event) {
    if (event.target.dataset.field !== 'provider') return;
    const provider = event.target.value;
    if (!Object.hasOwn(providers, provider)) return;
    state.provider = provider;
    notify(`已切换到 ${providers[provider]} 草稿。请填写或沿用本服务商自己的凭据；保存后才会切换后台配置。`);
    render();
  }

  root.addEventListener('click', onClick);
  root.addEventListener('input', onInput);
  root.addEventListener('change', onChange);
  render();
  return () => {
    root.removeEventListener('click', onClick);
    root.removeEventListener('input', onInput);
    root.removeEventListener('change', onChange);
  };
}
