#!/usr/bin/env python3
"""Throwaway terminal UI. Run: python3 prototypes/tui.py.

All configuration, credentials, DNS and service states are in-memory demos.
No files, DNS APIs, subprocesses or system services are accessed.
"""
import copy
import curses
import json
import locale
import time
import unicodedata


def columns(text):
    return sum(2 if unicodedata.east_asian_width(c) in "WF" else 1 for c in text)


def clipped(text, width):
    result = ""
    for char in str(text):
        if columns(result + char) > width:
            break
        result += char
    return result


class Prototype:
    def __init__(self):
        self.tab = 1
        self.focus = 0
        self.provider = "aliyun"
        self.domain = "home.example.com"
        self.credentials = {"aliyun": {"id": "DEMO_ONLY_RAM_ID", "secret": "DEMO_ONLY_SECRET"}, "cloudflare": {"token": ""}}
        self.mode = "stable"
        self.suffix = "::8023"
        self.startup = True
        self.running = True
        self.installed = True
        self.scenario = "正常"
        self.saved = self.configuration()
        self.synced = True
        self.last_run = "演示记录：刚刚"
        self.notice = "终端交互原型；请勿输入真实密钥。"

    def configuration(self):
        return copy.deepcopy({"provider": self.provider, "domain": self.domain, "credentials": self.credentials[self.provider], "mode": self.mode, "suffix": self.suffix, "startup": self.startup})

    def dirty(self):
        return self.configuration() != self.saved

    def fields(self):
        if self.tab == 0:
            return [("run", "模拟立即同步", "action"), ("scenario", "演示场景", "choice")]
        if self.tab == 1:
            result = [("provider", "DNS 服务商", "choice"), ("domain", "AAAA 域名", "text")]
            return result + ([("id", "AccessKey ID", "text"), ("secret", "AccessKey Secret", "secret")] if self.provider == "aliyun" else [("token", "API Token", "secret")])
        if self.tab == 2:
            return [("mode", "地址选择", "choice")] + ([("suffix", "IPv6 后缀", "text")] if self.mode == "suffix" else [])
        return [("startup", "开机自动启动", "check"), ("install", "安装 systemd 服务", "action"), ("service", "停止服务" if self.running else "启动服务", "action")]

    def value(self, key):
        if key in ("id", "secret", "token"):
            return self.credentials[self.provider][key]
        if key == "provider":
            return "阿里云 DNS" if self.provider == "aliyun" else "Cloudflare"
        if key == "mode":
            return "稳定公网 IPv6" if self.mode == "stable" else "按 IPv6 后缀筛选"
        return getattr(self, key, "")

    def put(self, key, value):
        if key in ("id", "secret", "token"):
            self.credentials[self.provider][key] = value
        else:
            setattr(self, key, value)

    def save(self):
        required = ("id", "secret") if self.provider == "aliyun" else ("token",)
        if not self.domain.strip() or any(not self.credentials[self.provider][key].strip() for key in required):
            self.notice = "未保存：请填写域名和当前服务商的演示凭据。"
            return
        if self.mode == "suffix" and not self.suffix.strip():
            self.notice = "未保存：请填写 IPv6 后缀。"
            return
        if self.dirty():
            self.synced = False
        self.saved = self.configuration()
        self.notice = "模拟保存完成；未同步 DNS，未安装或启动服务。"

    def run(self):
        if not self.running:
            self.notice = "未模拟同步：后台服务已停止；请在服务页显式启动。"
        elif self.scenario != "正常":
            self.notice = "未模拟同步：" + self.scenario + "。未调用 DNS。"
        else:
            self.synced = True
            self.last_run = time.strftime("%H:%M:%S") + "（模拟）"
            self.notice = "模拟同步已保存的 " + self.saved["domain"] + "；草稿不参与，未调用 DNS。"

    def activate(self):
        key, _, kind = self.fields()[self.focus]
        if key == "provider":
            self.provider = "cloudflare" if self.provider == "aliyun" else "aliyun"
        elif key == "mode":
            self.mode = "suffix" if self.mode == "stable" else "stable"
        elif key == "startup":
            self.startup = not self.startup
        elif key == "run":
            self.run()
        elif key == "scenario":
            names = ["正常", "DNS 权限错误", "无可用 IPv6"]
            self.scenario = names[(names.index(self.scenario) + 1) % len(names)]
            self.notice = "已切换演示场景：" + self.scenario
        elif key == "install":
            self.installed = True
            self.notice = "模拟服务已安装，未自动启动；没有执行 systemd 命令。"
        elif key == "service":
            if not self.installed:
                self.notice = "请先模拟安装服务。"
            else:
                self.running = not self.running
                self.notice = "后台服务已模拟" + ("启动" if self.running else "停止")
        elif kind in ("text", "secret"):
            return key, kind
        return None

    def paint(self, screen, editing=None):
        screen.erase()
        height, width = screen.getmaxyx()

        def line(y, x, text, style=0):
            if 0 <= y < height - 1 and x < width - 1:
                screen.addstr(y, x, clipped(str(text), width - x - 1), style)

        if width < 76 or height < 24:
            line(0, 0, "请将终端调整到至少 76 列 × 24 行；q 退出。")
            screen.refresh()
            return
        line(0, 1, "IPv6 DDNS / Linux TUI                         交互原型 · 非正式功能", curses.A_BOLD)
        line(1, 1, "仅内存演示：不写配置、不调用 DNS、不操作服务；请勿输入真实密钥。")
        service = "运行中" if self.running else "已停止"
        line(3, 1, "后台服务（模拟）：" + service + "    配置：" + ("* 未保存" if self.dirty() else "已保存") + "    场景：" + self.scenario)
        x = 1
        for index, title in enumerate(["1 状态", "2 DNS 配置", "3 IPv6", "4 服务"]):
            label = "[ " + title + " ]"
            line(5, x, label, curses.A_BOLD | curses.A_UNDERLINE if self.tab == index else 0)
            x += columns(label) + 2
        line(6, 1, "─" * (width - 3))
        fields = self.fields()
        for index, (key, label, kind) in enumerate(fields):
            y = 8 + index * 2
            active = index == self.focus
            line(y, 1, ">" if active else " ", curses.A_BOLD)
            line(y, 3, label)
            value = self.value(key)
            if editing and editing[0] == key:
                value = editing[2]
            if kind == "secret":
                value = "*" * len(value)
            if kind == "choice":
                value = "< " + value + " >"
            elif kind == "check":
                value = "[x] 开启" if value else "[ ] 关闭"
            elif kind == "action":
                value = "[ Enter 执行（模拟） ]"
            else:
                value = "[ " + clipped(value, width - 32) + " ]"
            line(y, 24, value, curses.A_BOLD if active else 0)
        if self.tab == 1:
            line(16, 3, "← / → 切换服务商；Enter 编辑字段；阿里云使用 RAM 用户凭据。")
        elif self.tab == 2:
            line(13, 3, "当前示例地址：2001:db8::8023（不是实际检测结果）")
            line(15, 3, "稳定模式跳过临时隐私地址；修改策略后需保存。")
        elif self.tab == 3:
            line(15, 3, "ipv6-ddns.service · " + ("已安装" if self.installed else "未安装") + "（模拟）；正式服务管理需管理员权限。")
            line(16, 3, "退出 TUI 不停止服务。首期目标：systemd 发行版。")
        else:
            line(13, 3, "上次同步：" + self.last_run + "；" + ("已同步（模拟）" if self.synced else "新配置等待同步"))
            line(15, 3, "检查周期：5 分钟；失败重试：1 分钟（原型不执行定时任务）")
        provider = "阿里云" if self.saved["provider"] == "aliyun" else "Cloudflare"
        line(height - 6, 1, "已保存：" + provider + " / " + self.saved["domain"] + " / AAAA")
        line(height - 4, 1, self.notice, curses.A_BOLD)
        line(height - 2, 1, "1–4 切页  Tab/↑↓ 焦点  ←→ 切换  Enter 编辑  Space 勾选  Ctrl+S 保存  F5 同步  q 退出")
        if editing:
            line(height - 3, 1, "编辑中：Enter 确认草稿 · Esc 取消 · Ctrl+U 清空；输入不会自动保存。")
        screen.refresh()

    def edit(self, screen, key, kind):
        original = self.value(key)
        value = original
        while True:
            self.paint(screen, (key, kind, value))
            char = screen.get_wch()
            if char in ("\n", "\r", curses.KEY_ENTER):
                self.put(key, value)
                self.notice = "字段已修改为草稿；Ctrl+S 才模拟保存。"
                return
            if char == "\x1b":
                self.notice = "已取消编辑；字段保持原值。"
                return
            if char == "\x15":
                value = ""
            elif char in (curses.KEY_BACKSPACE, "\x7f", "\b"):
                value = value[:-1]
            elif isinstance(char, str) and char.isprintable():
                value += char

    def main(self, screen):
        curses.curs_set(0)
        if curses.has_colors():
            curses.start_color()
            curses.use_default_colors()
            curses.init_pair(1, -1, -1)
            screen.bkgd(" ", curses.color_pair(1))
            screen.attrset(curses.color_pair(1))
        screen.keypad(True)
        curses.raw()
        curses.set_escdelay(25)
        while True:
            self.focus = min(self.focus, len(self.fields()) - 1)
            self.paint(screen)
            char = screen.get_wch()
            if char in ("q", "Q", "\x03"):
                return
            if isinstance(char, str) and char in "1234":
                self.tab, self.focus = int(char) - 1, 0
            elif char in ("\t", curses.KEY_DOWN):
                self.focus = (self.focus + 1) % len(self.fields())
            elif char in (curses.KEY_BTAB, curses.KEY_UP):
                self.focus = (self.focus - 1) % len(self.fields())
            elif char == "\x13":
                self.save()
            elif char == curses.KEY_F5:
                self.run()
            elif char == "\x1b":
                self.tab, self.focus = 0, 0
            elif char in ("\n", "\r", curses.KEY_ENTER, " ", curses.KEY_LEFT, curses.KEY_RIGHT):
                kind = self.fields()[self.focus][2]
                if char in (curses.KEY_LEFT, curses.KEY_RIGHT) and kind != "choice":
                    continue
                result = self.activate()
                if result:
                    self.edit(screen, *result)


if __name__ == "__main__":
    locale.setlocale(locale.LC_ALL, "")
    prototype = Prototype()
    print("TUI PROTOTYPE READY — no real DNS or service operations", flush=True)
    curses.wrapper(prototype.main)
    print("已退出 TUI 原型；模拟后台服务仍为" + ("运行中" if prototype.running else "已停止") + "。未操作真实系统服务。")
    print(json.dumps({"provider": prototype.saved["provider"], "domain": prototype.saved["domain"], "startup": prototype.saved["startup"], "unsaved": prototype.dirty(), "service_running": prototype.running, "synced": prototype.synced}, ensure_ascii=False))
