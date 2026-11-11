import { mountDesktop } from './desktop.js';

// Browser-only, throwaway prototypes. No production bundle imports this entry.
const variants = {
  A: ['紧凑原生 · 推荐', '延续现有小窗口：日常只看同步状态，需要时展开配置。'],
  B: ['分区工作台', '导航与内容分区：状态、DNS 和 IPv6 设置各自独立；适合经常检查配置与日志。'],
  C: ['分步引导', '按「DNS 凭据 → IPv6 → 确认」完成配置；适合首次安装，日常回到运行状态。'],
};
const platforms = {
  mac: ['macOS / 菜单栏', '沿用独立后台守护进程。窗口关闭后留在菜单栏；服务不依赖用户登录。此处为浏览器外观模拟。'],
  win: ['Windows / 系统托盘', '沿用 Windows 服务与托盘窗口。关闭设置仅隐藏窗口，后台同步继续。此处为浏览器外观模拟。'],
  linux: ['Linux / 真实终端 TUI', '不再使用网页模拟终端控件。配置原型改为 Python curses，在实际终端中运行；不写配置、不调用 DNS、不管理真实服务。'],
};
const scenarios = ['healthy', 'unconfigured', 'auth-error', 'no-ipv6', 'stopped'];
let cleanup;
function current() {
  const params = new URLSearchParams(location.search);
  return {
    platform: Object.hasOwn(platforms, params.get('platform')) ? params.get('platform') : 'mac',
    variant: Object.hasOwn(variants, params.get('variant')) ? params.get('variant') : 'A',
    scenario: scenarios.includes(params.get('scenario')) ? params.get('scenario') : 'healthy',
  };
}
function masked(value, key = '') {
  if (/token|secret|accesskey|credential/i.test(key) && typeof value === 'string') return value ? '[已隐藏]' : '[空]';
  if (Array.isArray(value)) return value.map(item => masked(item));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, masked(v, k)]));
  return value;
}
function render() {
  cleanup?.();
  const selection = current();
  const terminal = selection.platform === 'linux';
  document.querySelector('.prototype-switcher').hidden = terminal;
  document.querySelector('.scenario-picker').hidden = terminal;
  document.documentElement.dataset.platform = selection.platform;
  document.title = `IPv6 DDNS · ${selection.platform} · 方案 ${selection.variant}（原型）`;
  for (const button of document.querySelectorAll('[data-platform]')) button.setAttribute('aria-pressed', String(button.dataset.platform === selection.platform));
  for (const button of document.querySelectorAll('[data-variant]')) button.setAttribute('aria-pressed', String(button.dataset.variant === selection.variant));
  document.querySelector('#scenario').value = selection.scenario;
  document.querySelector('#variant-title').textContent = `${selection.variant} / ${variants[selection.variant][0]}`;
  document.querySelector('#variant-label').textContent = `${selection.variant} · ${variants[selection.variant][0]}`;
  document.querySelector('#variant-note').textContent = variants[selection.variant][1];
  document.querySelector('#platform-title').textContent = platforms[selection.platform][0];
  document.querySelector('#platform-note').textContent = platforms[selection.platform][1];
  const root = document.querySelector('#surface');
  root.replaceChildren();
  if (terminal) {
    cleanup = undefined;
    const panel = document.createElement('section');
    panel.className = 'terminal-preview';
    const heading = document.createElement('h2');
    heading.textContent = 'Linux 原型请直接在终端确认';
    const command = document.createElement('pre');
    command.textContent = 'python3 prototypes/tui.py';
    const help = document.createElement('p');
    help.textContent = '在项目目录运行。1–4 切页，Tab / ↑↓ 移动焦点，←→ 切换选项，Enter 编辑，Ctrl+S 模拟保存，F5 模拟同步，q 退出。';
    panel.append(heading, command, help);
    root.append(panel);
    document.querySelector('#variant-title').textContent = '真实终端交互';
    document.querySelector('#variant-note').textContent = '字符控件与键盘操作，不使用浏览器 select。';
    document.querySelector('#state').textContent = '请在运行中的终端内查看状态；网页不再维护 Linux 模拟状态。';
    return;
  }
  cleanup = mountDesktop(root, { ...selection, onState: state => {
    document.querySelector('#state').textContent = JSON.stringify({ preview: true, ...selection, ...masked(state) }, null, 2);
  }});
}
function navigate(changes) {
  const next = { ...current(), ...changes };
  const params = new URLSearchParams(location.search);
  for (const [key, value] of Object.entries(next)) params.set(key, value);
  history.pushState(null, '', `${location.pathname}?${params}`);
  render();
}
function cycle(delta) {
  const names = Object.keys(variants);
  navigate({ variant: names[(names.indexOf(current().variant) + delta + names.length) % names.length] });
}
document.querySelectorAll('[data-platform]').forEach(button => button.addEventListener('click', () => navigate({ platform: button.dataset.platform })));
document.querySelectorAll('[data-variant]').forEach(button => button.addEventListener('click', () => navigate({ variant: button.dataset.variant })));
document.querySelector('#scenario').addEventListener('change', event => navigate({ scenario: event.target.value }));
document.querySelector('#previous').addEventListener('click', () => cycle(-1));
document.querySelector('#next').addEventListener('click', () => cycle(1));
document.addEventListener('keydown', event => {
  if (current().platform === 'linux') return;
  if (event.target.closest('input, textarea, select, [contenteditable="true"]') || event.ctrlKey || event.metaKey || event.altKey) return;
  if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
    event.preventDefault();
    cycle(event.key === 'ArrowLeft' ? -1 : 1);
  }
});
window.addEventListener('popstate', render);
render();
