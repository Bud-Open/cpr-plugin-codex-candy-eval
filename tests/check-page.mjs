"use strict";
// 校验 ui.html 的 CPR 宿主资源装配约束；在 jsdom 中以内联脚本和模拟桥验证渲染与交互。
// 这是本地兼容性检查，不替代真实 CPR iframe/CSP/桥运行时的浏览器验收。
//
// 用法：node tests/check-page.mjs <plugin-dir>

import { readFileSync } from "node:fs";
import path from "node:path";
import { JSDOM } from "jsdom";
import * as csstree from "css-tree";
import { buildPage } from "../tools/build-page.mjs";

const root = path.resolve(process.argv[2] || ".");
const manifest = JSON.parse(readFileSync(path.join(root, "plugin.json"), "utf8"));
const resources = new Map();
for (const [rel, mime] of Object.entries(manifest.resources)) {
  resources.set(rel, { mime, body: readFileSync(path.join(root, rel)) });
}

const failures = [];
const fail = (message) => failures.push(message);
const check = (condition, message) => { if (!condition) fail(message); };

const PATH_SEGMENT = /^(?!\.{1,2}$)[\w.-]+$/u;
const JAVASCRIPT = new Set(["application/ecmascript", "application/javascript", "text/ecmascript", "text/javascript"]);
const mime = (value) => String(value).split(";")[0].trim().toLowerCase();

function resolveReference(ownerPath, raw) {
  const reference = String(raw).trim();
  if (!reference || reference.startsWith("/") || reference.startsWith("//") || reference.includes("\\")
    || reference.includes("%") || reference.includes("?") || reference.includes("#")
    || /^[a-z][a-z\d+.-]*:/iu.test(reference)) {
    throw new Error(`资源地址不受支持：${raw}`);
  }
  const base = ownerPath.split("/").slice(0, -1);
  for (const segment of reference.split("/")) {
    if (!segment || segment === ".") continue;
    if (segment === "..") { if (!base.length) throw new Error(`越过制品边界：${raw}`); base.pop(); continue; }
    if (!PATH_SEGMENT.test(segment)) throw new Error(`路径片段无效：${raw}`);
    base.push(segment);
  }
  const resolved = base.join("/");
  if (!resources.has(resolved)) throw new Error(`引用了未注册资源：${resolved}`);
  return resolved;
}

const dataUrl = (body, contentType) => `data:${contentType};base64,${body.toString("base64")}`;

function rewriteCss(source, ownerPath, allowImports = true) {
  const tree = csstree.parse(source, {
    context: allowImports ? "stylesheet" : "declarationList",
    parseRulePrelude: false,
    parseCustomProperty: true,
    onParseError(_error, fallback) { if (fallback) throw new Error(`CSS 语法不受支持：${ownerPath}`); },
  });
  const imported = new WeakSet();
  csstree.walk(tree, function (node) {
    if (node.type === "Raw" && node !== this.rule?.prelude) throw new Error(`CSS 含无法解析内容：${ownerPath}`);
    if ((node.type === "Atrule" || node.type === "Function") && node.name.includes("\\")) throw new Error(`CSS 规则名转义：${ownerPath}`);
    if (node.type === "Atrule" && node.name.toLowerCase() === "import") {
      if (!allowImports) throw new Error("内联 style 不能 @import");
      const reference = node.prelude?.type === "AtrulePrelude" ? node.prelude.children.first : null;
      if (node.block || !reference || (reference.type !== "String" && reference.type !== "Url")) throw new Error(`@import 语法不受支持：${ownerPath}`);
      const resolved = resolveReference(ownerPath, reference.value);
      if (mime(resources.get(resolved).mime) !== "text/css") throw new Error(`@import 目标不是 CSS：${resolved}`);
      reference.value = dataUrl(resources.get(resolved).body, "text/css");
      imported.add(reference);
    }
    if (node.type === "Url" && !imported.has(node)) {
      if (node.value.startsWith("#")) return;
      const resolved = resolveReference(ownerPath, node.value);
      node.value = dataUrl(resources.get(resolved).body, resources.get(resolved).mime);
    }
  });
  return csstree.generate(tree);
}

// ---------------------------------------------------------------- 装配校验

const entry = "web/ui.html";
const entryBody = buildPage(root);
resources.set(entry, { mime: "text/html", body: Buffer.from(entryBody) });
const document = new JSDOM(entryBody).window.document;
check(![...resources.keys()].some((resource) => resource.endsWith(".js")), "发布清单不能预取独立脚本资源");
check(!document.querySelector("script[src]"), "发布入口必须内联全部脚本");
check(Buffer.byteLength(entryBody) <= 1024 * 1024, "发布入口超过 CPR 1 MiB 限制");

const unsupported = document.querySelector("base, iframe, frame, frameset, object, embed, portal, template");
check(!unsupported, `页面使用了不受支持的元素：${unsupported?.tagName}`);
check(![...document.querySelectorAll("meta[http-equiv]")].some((meta) => meta.getAttribute("http-equiv")?.toLowerCase() === "refresh"), "页面不能自动跳转");

for (const script of document.querySelectorAll("script")) {
  check(script.getAttribute("type")?.trim().toLowerCase() !== "module", "页面不能使用 ES Module");
  const source = script.getAttribute("src");
  if (!source) continue;
  try {
    const resolved = resolveReference(entry, source);
    check(JAVASCRIPT.has(mime(resources.get(resolved).mime)), `脚本资源类型无效：${resolved}`);
  } catch (error) { fail(error.message); }
}

for (const link of document.querySelectorAll("link[href]")) {
  const rel = new Set((link.getAttribute("rel") ?? "").toLowerCase().split(/\s+/u).filter(Boolean));
  check([...rel].every((value) => value === "stylesheet" || value === "icon") && rel.size > 0, `只支持 stylesheet/icon 链接：${link.outerHTML}`);
  try {
    const resolved = resolveReference(entry, link.getAttribute("href"));
    const resolvedMime = mime(resources.get(resolved).mime);
    if (rel.has("stylesheet")) check(resolvedMime === "text/css", `样式资源类型无效：${resolved}`);
    if (resolvedMime === "text/css") rewriteCss(resources.get(resolved).body.toString("utf8"), resolved);
  } catch (error) { fail(error.message); }
}

const supportedSource = new Set(["AUDIO", "IMG", "SCRIPT", "SOURCE", "TRACK", "VIDEO"]);
for (const element of document.querySelectorAll("[src]")) {
  if (element.tagName === "SCRIPT") continue;
  check(supportedSource.has(element.tagName), `不支持 <${element.tagName.toLowerCase()} src>`);
  try { resolveReference(entry, element.getAttribute("src")); } catch (error) { fail(error.message); }
}

for (const element of document.querySelectorAll("*")) {
  for (const attribute of [...element.attributes]) {
    const name = attribute.name.toLowerCase();
    if (name === "href" || name === "xlink:href") {
      if (element.tagName === "LINK" || attribute.value.startsWith("#")) continue;
      fail(`页面不能导航到外部地址：<${element.tagName.toLowerCase()} href="${attribute.value}">`);
    }
    if (["action", "formaction", "ping", "background"].includes(name) && attribute.value) fail(`不支持 ${name} 地址`);
    if (name !== "style" && /\burl\s*\(/iu.test(attribute.value) && !/^url\(\s*#[\w.-]+\s*\)$/iu.test(attribute.value.trim())) {
      fail(`属性不能引用外部资源：${name}`);
    }
  }
}
for (const style of document.querySelectorAll("style")) {
  try { rewriteCss(style.textContent ?? "", entry, false); } catch (error) { fail(error.message); }
}

// ---------------------------------------------------------------- 沙箱渲染

const GLOBALS_BLOCKED = ["fetch", "XMLHttpRequest", "WebSocket", "localStorage", "sessionStorage"];

const calls = [];
let mockAnswers = [true, false, true];

function mockBackend(call) {
  // 真实插件状态里失败记录的字段集：没有 attribution，samples 为空数组。
  const mtRecord = {
    id: "mt-1", bank_revision: "rev1", status: "error", model: "gpt-6.1-sol",
    time: new Date(Date.now() - 60000).toISOString(), duration_ms: 7680, concurrency: 3,
    input_tokens: 0, output_tokens: 0, reasoning_tokens: 0, samples: [],
    error: "模型请求失败：no upstream provider is currently available for this request",
  };
  const credential = {
    id: "acct-1", provider: "codex", source: "cpr_accounts", name: "主账号",
    provider_name: "codex", email: "a@example.invalid", plan_type: "",
    disabled: false, unavailable: false, status_message: "",
    running: null, results: [], pelicans: [], fingerprints: [], modeltraces: [mtRecord],
  };
  const routes = {
    "GET api/accounts": () => ({ accounts: [credential], revision: "rev1" }),
    "GET api/models": () => ({ data: [{ id: "gpt-6.1-sol" }, { id: "gpt-6.1-sol(xhigh)" }] }),
    "GET api/accounts/models": () => ({ models: [{ id: "gpt-6.1-sol" }] }),
    "POST api/run": () => ({ started: 1, skipped: 0, unchecked: 0, busy: 0 }),
    "GET api/modeltrace/record": () => mtRecord,
  };
  if (`${call.method} ${call.path}` === "GET api/state") {
    const results = mockAnswers.map((ok, index) => ({
      skipped: false, time: new Date(Date.now() - index * 1000).toISOString(), model: "gpt-6.1-sol",
      effort: "low", ok, answer: ok ? "21" : "22", error: "", input_tokens: 10,
      output_tokens: 5, reasoning_tokens: 0, duration_ms: 1200,
    }));
    return { status: 200, body: { auths: [{ ...credential, results }], models: ["gpt-6.1-sol"], storage_error: "", version: "1.0.0" } };
  }
  const handler = routes[`${call.method} ${call.path}`];
  if (!handler) return { status: 404, body: { error: { message: `未注册路由：${call.method} ${call.path}` } } };
  return { status: 200, body: handler() };
}

// 复刻 frame.ts 的装配结果：脚本与样式内联成页面自带内容，桥脚本前置注入 <head>。
// 宿主用 data: URL 承载子资源；jsdom 不会加载 data: 脚本，这里改为等价的内联脚本。
const rewritten = new JSDOM(entryBody);
for (const script of [...rewritten.window.document.querySelectorAll("script[src]")]) {
  const resolved = resolveReference(entry, script.getAttribute("src"));
  script.removeAttribute("src");
  script.textContent = resources.get(resolved).body.toString("utf8");
}
for (const link of [...rewritten.window.document.querySelectorAll("link[rel~=stylesheet]")]) {
  const resolved = resolveReference(entry, link.getAttribute("href"));
  const style = rewritten.window.document.createElement("style");
  style.textContent = rewriteCss(resources.get(resolved).body.toString("utf8"), resolved);
  link.replaceWith(style);
}
const bridgeSource = `
(function () {
  const api = {
    version: 2,
    plugin: Object.freeze({ name: "nmbb.codex-candy-eval" }),
    page: Object.freeze({ id: "codex-candy-eval", title: "Codex 降智与指纹测试" }),
    theme: Object.freeze({ name: "dark", tokens: {} }),
    resourceUrl(path) { throw new Error("页面不应直接加载资源：" + path); },
    callbackTicket() { return Promise.reject(new Error("未使用")); },
    models: Object.freeze({ responses() { return Promise.reject(new Error("未使用")); } }),
    request(input) {
      const call = { method: String(input.method || "").toUpperCase(), path: String(input.path || ""), query: input.query == null ? "" : String(input.query) };
      window.__calls.push(call);
      const answer = window.__backend(call);
      return Promise.resolve({
        status: answer.status,
        contentType: "application/json",
        body: new TextEncoder().encode(JSON.stringify(answer.body)).buffer,
      });
    },
  };
  Object.defineProperty(window, "codexProxyPlugin", { value: Object.freeze(api), configurable: false, writable: false });
  document.documentElement.dataset.theme = "dark";
})();
`;
const bridge = rewritten.window.document.createElement("script");
bridge.textContent = bridgeSource;
rewritten.window.document.head.prepend(bridge);
const srcdoc = `<!doctype html>\n${rewritten.window.document.documentElement.outerHTML}`;

const errors = [];
const dom = new JSDOM(srcdoc, {
  runScripts: "dangerously",
  pretendToBeVisual: true,
  url: "https://plugin.invalid/",
  beforeParse(window) {
    window.__calls = calls;
    window.__backend = mockBackend;
    // jsdom 未实现 <dialog>；按宿主资源 CSP 的沙箱语义补齐：只有非模态 show() 可用。
    const dialogProto = window.HTMLDialogElement.prototype;
    dialogProto.show = function () { this.setAttribute("open", ""); };
    dialogProto.showModal = function () {
      throw new window.DOMException("The document is sandboxed and lacks the 'allow-modals' keyword.", "SecurityError");
    };
    dialogProto.close = function () { this.removeAttribute("open"); this.dispatchEvent(new window.Event("close")); };
    // jsdom 未实现 CSS.escape；浏览器里始终存在，这里给一个等价的属性值转义实现。
    window.CSS = { escape: (value) => String(value).replace(/[^a-zA-Z0-9_-]/g, (ch) => "\\" + ch) };
    // jsdom 未实现 matchMedia；浏览器里始终存在，这里补一个最小替身。
    window.matchMedia = (query) => ({
      matches: false, media: query, onchange: null,
      addEventListener() {}, removeEventListener() {},
      addListener() {}, removeListener() {}, dispatchEvent() { return false; },
    });
    window.addEventListener("error", (event) => errors.push(String(event.error?.stack || event.message)));
    window.addEventListener("unhandledrejection", (event) => errors.push(`未处理的 Promise 拒绝：${event.reason}`));
    // 沙箱页面没有网络与存储，任何直接使用都是缺陷。
    for (const name of GLOBALS_BLOCKED) {
      Object.defineProperty(window, name, {
        configurable: true,
        get() { throw new Error(`沙箱页面不应直接使用 ${name}`); },
      });
    }
  },
});

const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
await wait(3000);

const doc = dom.window.document;
const $ = (id) => doc.getElementById(id);

for (const error of errors) fail(`页面抛出异常：${error}`);
check($("app") && !$("app").hidden, "主界面未显示");
check($("login") && $("login").hidden, "不应要求管理密钥登录");
check(doc.documentElement.dataset.cprPluginHost === "", "未标记 CPR 宿主，主题样式不会生效");

const options = [...$("model").options].map((option) => option.value);
check(options.includes("gpt-6.1-sol"), `模型下拉缺少目标模型：${JSON.stringify(options)}`);

const rows = $("rows")?.children?.length ?? 0;
check(rows === 1, `凭证行数量应为 1，实际 ${rows}`);
// 控制台用邮箱作为凭证标题，与 CPA 面板一致。
check(($("rows")?.textContent || "").includes("a@example.invalid"), "凭证行未渲染账号标识");
check((dom.window.document.body.textContent || "").includes("CPR 账号"), "账号筛选未使用 CPR 来源标签");
// 糖果判定与最近记录都在行内渲染。
check(!!$("rows")?.querySelector(".verdict.ok"), "糖果判定未渲染为答对");
// 行内汇总：判定、耗时与正确率。
const rowText = $("rows")?.textContent || "";
check(rowText.includes("答对"), "糖果判定未渲染");
check(rowText.includes("67%"), "糖果正确率未渲染");

// 页面只保留糖果、ModelTrace、鹈鹕三套件；指纹页签已下架，不能重新出现在沙箱里。
check(!$("tab-fingerprint"), "指纹页签应已从页面移除");
for (const suite of ["modeltrace", "pelican", "candy"]) {
  $("tab-" + suite)?.dispatchEvent(new dom.window.MouseEvent("click", { bubbles: true }));
  check(!$(suite + "-panel")?.hidden, `${suite} 面板未显示`);
  check($("tab-" + suite)?.getAttribute("aria-selected") === "true", `${suite} 页签未选中`);
}

// 沙箱（sandbox allow-scripts）禁用 modal：详情必须用非模态对话框打开，且失败原因可见。
$("tab-modeltrace")?.dispatchEvent(new dom.window.MouseEvent("click", { bubbles: true }));
await wait(50);
$("mt-rows")?.querySelector("[data-row]")?.dispatchEvent(new dom.window.MouseEvent("click", { bubbles: true }));
await wait(50);
const detailButton = $("mt-rows")?.querySelector("[data-mt-detail]");
check(!!detailButton, "ModelTrace 历史记录缺少查看详情按钮");
detailButton?.dispatchEvent(new dom.window.MouseEvent("click", { bubbles: true }));
await wait(400);
check($("mt-detail")?.open === true, "查看详情未能打开详情对话框");
check(!!doc.querySelector(".dialog-backdrop"), "非模态对话框缺少遮罩");
check(($("mt-detail-body")?.textContent || "").includes("no upstream provider"), "详情未显示失败原因");
$("mt-detail-close")?.dispatchEvent(new dom.window.MouseEvent("click", { bubbles: true }));
await wait(20);
check($("mt-detail")?.open === false, "关闭详情后对话框仍处于打开状态");
check(!doc.querySelector(".dialog-backdrop"), "关闭后遮罩残留");
const callsSeen = new Set(calls.map((call) => `${call.method} ${call.path}`));
for (const expected of [
  "GET api/accounts",
  "GET api/models",
  "GET api/state",
]) {
  check(callsSeen.has(expected), `页面未调用预期路由：${expected}`);
  check(![...callsSeen].some((route) => route.includes("v0/management") || route.includes("v8/management") || route.includes("credentials/sync")), "新页面不应再依赖 CPA 管理接口别名");
}

const registered = new Set(JSON.parse(readFileSync(path.join(root, "tests", "routes.json"), "utf8")).map(([method, route]) => `${method} ${route}`));
for (const key of callsSeen) check(registered.has(key), `后端未注册页面调用的路由：${key}`);

const runButton = $("run-batch");
check(runButton && !runButton.disabled, "测试按钮应可用");
runButton?.dispatchEvent(new dom.window.MouseEvent("click", { bubbles: true }));
await wait(800);
check(calls.some((call) => call.path === "api/run"), "点击测试未触发 api/run");
for (const error of errors) fail(`页面抛出异常：${error}`);
// 关闭页面轮询与三个 jsdom 窗口，避免校验通过后进程仍常驻。
dom.window.close();
rewritten.window.close();
document.defaultView.close();
if (failures.length) {
  console.error("页面装配/渲染校验失败：");
  for (const failure of failures) console.error("  - " + failure);
  process.exit(1);
}
console.log(`页面校验通过：资源 ${resources.size} 项，桥调用 ${calls.length} 次，凭证行 ${rows} 行，模型 ${options.length} 个`);
console.log("调用的路由：" + [...callsSeen].sort().join(", "));
