# CPR Codex 糖果自检插件

**CPR（codex-proxy-rs）原生 Rust 插件**：把 CPA 平台上的 [`cpa-plugin-codex-candy-eval`](https://github.com/haowang02/cpa-plugin-codex-candy-eval)（Go + CGO 动态库）的检测能力，重写为运行在 CPR `gateway-plugin-sdk` 上的独立子进程插件，用于自查 Codex 账号是否“降智”、模型指纹是否被替换、以及模型在长输出与指令遵循上的退化。

> 本仓库是**魔改移植版**，不是上游原作者发布的产品。来源与许可见 [NOTICE.md](NOTICE.md) 与 [LICENSE](LICENSE)。

## 功能套件

界面顶部页签切换，四套件共用同一套模型调用链路（可指定账号、模型与推理强度）：

| 套件 | 用途 | 结果口径 |
| --- | --- | --- |
| **糖果测试** `candy` | 向账号发同一道题并比对标准答案，最直接的“降智”判据 | 答对 / 答错 / 请求出错 / 已跳过，按“模型+推理强度”单独统计正确率与最近 20 次分布 |
| **深度追踪** `modeltrace` | 按 ModelTrace 题库采集样本，与参考基线比对 | 最接近的候选模型与概率、家族概率、有效样本数；历史记录按题库 SHA-256 摘要归档 |
| **鹈鹕绘图** `pelican` | 无工具生成 SVG 自行车动画 | 直接渲染生成的动画页面，观察长输出与指令遵循是否退化 |
| **模型指纹** `fingerprint` | 多轮探针 + 基线比对，判断实际服务的模型与声明是否一致 | 当前版本页面**未提供入口**（引擎与管理路由保留，供后续启用） |

## 与上游的主要差异（魔改点）

- **运行时**：Go + CGO 动态库 → **纯 Rust 子进程插件**（Tokio 异步 IPC，`runtime: trustedProcess`），除宿主 ABI 外不依赖共享库，不需要与宿主同编译器/同 libc 构建。
- **账号来源**：CPR 通过 `account_facts` 暴露账号事实，插件直接读取，**不在插件内维护凭据文件**；读取失败时显式报错，不会伪装成“0 个账号”。
- **模型调用**：按 CPR 模型执行契约发起——显式携带 Client Key 的**非秘密标识符**作为执行身份、显式指定账号与 provider 协议；管理页自检不再因缺少父请求上下文被宿主拒绝。
- **推理强度语义**：空推理强度按上游原意**整键省略**（交回宿主与上游默认值），不发送空字符串。
- **前端**：单页内联打包（无外链脚本请求，适配宿主沙箱 CSP），对话框保留非模态 `show()` 兼容路径并按父页面实际可见区域定位；嵌入模式不使用视口最小高度，避免 iframe 自动高度反馈循环；运行期间持续轮询状态显示进度。
- **取消语义**：取消后立即放弃在途样本并结束状态机，不无限期等待上游响应。
- **工程化**：逐版本离线制品（官方 `cpr-plugin package`，含逐文件摘要）、页面契约测试与真实 IPC 帧契约测试。
- **历史持久化（v1.0.12+）**：官方状态 KV 分块写入（每块 16 KiB），双缓冲完整写入后才 CAS 提交索引；读取校验长度与 SHA-256，写入串行化，避免把 240 KiB 快照塞进 SDK 64 KiB 元数据帧。旧 `global` 键保留不改；每账号每套件最近 20 条及快照预算维持原规则。
- **恢复一致性（v1.0.13+）**：记录按稳定 ID 核验后合并；正文、样本或时间冲突会拒绝覆盖。开启 JSON 精确浮点往返，兼容已有记录的机器精度级舍入差异；无 ID 的糖果结果保留重复次数，防止把旧结果恢复成重复新记录。

### 运行通知契约（v1.0.11）

CPR 管理页的 `/run` 请求在当前宿主调用作用域内执行到结束才返回，不能按“提交后立即返回”的后台任务接口处理。前端点击后立即显示准备提示，提交后显示等待执行结果，响应后提示本轮执行结束；“结束”不等于全部答对，具体结果见历史记录。

响应中 `started` 是执行过的凭证数量，`skipped`、`unchecked`、`busy` 是账号标识数组。通知只使用数组长度，不拼接账号标识；空数组不产生跳过提示或黄色告警。`unchecked` 在现有后端表示未确认支持所选模型、实际已跳过，不能显示“仍会尝试”。为兼容旧测试与调用方，非负整数计数也可识别。

页面契约测试使用真实数组结构和可控的延迟响应，覆盖糖果、ModelTrace、鹈鹕三条执行路由，断言响应前后提示、空数组、正计数、零执行项、重复点击抑制和 HTTP 错误。以前的立即响应 + 全数字 Mock 没有覆盖这类跨层契约和时序问题。

## 构建与测试

依赖：Rust 工具链、Node.js（页面契约测试）、官方 `cpr-plugin` CLI（打包）。

```bash
# 单元与契约测试（含握手、驱动、账号契约、模型执行契约）
cargo test --offline --locked
# 使用 Rustup 的当前工具链；不要使用无法解析当前锁文件的系统旧 Cargo。
# 发布脚本默认优先使用 $HOME/.cargo/bin/cargo。

# 页面契约测试：在 jsdom 中校验内联资源、沙箱桥调用与交互断言
npm install            # 首次需要 jsdom 与 css-tree
node tests/check-page.mjs .

# 构建并打包离线制品（自动跑 fmt/test/页面校验/内联打包/release 构建）
bash tools/package-release.sh
# 输出：dist/native-<version>/nmbb.codex-candy-eval-<version>-x86_64-unknown-linux-gnu.tar.gz
```

### 浏览器布局回归（v1.0.10）

`jsdom` 不计算真实布局，不能证明 iframe 高度或弹窗位置稳定。使用 `agent-browser` 打开真实宿主插件页后，可在仅绑定回环的脱敏联调环境中执行：

```bash
python3 -m unittest discover -s tests -p 'test_browser_proxy.py'
# 额外依赖：websocket-client；端口取 agent-browser 管理的诊断 Chrome CDP 监听端口
python3 tests/check-browser-layout.py --port <cdp-port>
```

检查包括连续高度采样、ModelTrace/鹈鹕详情各三次开关、弹窗完整位于父页面可见区域、遮罩释放与账号页导航。它只读取既有记录，不启动模型评测；`--candidate` 仅在该诊断浏览器 DOM 中注入本地候选样式与弹窗逻辑，不修改线上制品。自动化覆盖不替代用户设备的交互验收。

> `sandbox="allow-scripts"` 并不禁止 HTML `<dialog>.showModal()`。`allow-modals` 约束的是 `alert`、`confirm`、`prompt` 等浏览器模态 API，不应据此推断 `<dialog>` 会抛权限异常。

## 安装与升级

目标主机需要 CPR `>= 3.25.0, < 4.0.0`。

1. 通过 CPR 管理接口上传制品：`POST /api/admin/plugins/artifacts/upload`；
2. 用 `switch-version` 把既有实例原子切换到新版本（实例 ID 不变，状态与历史记录保留）；
3. 校验实例状态：`running: true`、无 `loadError`、无兼容性警告，资源与路由数量符合预期；
4. 同持久化格式版本出现异常时回退上一制品。**跨 v1.0.12 格式降级必须同时恢复升级前插件状态备份及配额，不能只切旧二进制**：新版使用多键双缓冲，状态配额为 40 条 / 2 MiB，旧版仅 10 条 / 256 KiB。升级前须备份插件数据及数据库；恢复操作应先核对备份和目标实例。

`tools/remote-*.py` 是在**目标主机上**运行的运维辅助脚本（制品上传与切换、实例重载、状态快照、真实跑测、取消在途任务、浏览器联调代理），只输出聚合计数，不打印凭据。`cpr-candy-inspect.sh` 为宿主机侧巡检脚本（本地回环读取管理密钥，仅输出数量与状态）。

## 目录结构

```
src/      插件实现：app 入口、command 指令分发、management 管理路由、host_calls 宿主 IPC、
          state 状态机与合并恢复、storage 原生 KV 分块快照、model 数据模型、codex 请求构造、四套件与 runs 调度
web/      前端界面：ui.html 单页（构建时内联全部脚本）、app/catalog/components 等模块、vendor 第三方库
data/     编译期嵌入的题面与基线（codex_context、指纹探针与基线、ModelTrace 题库）
tests/    Rust 契约测试 + jsdom 页面契约 + Chrome OOPIF 几何回归 + 代理认证/脱敏测试 + routes.json 路由清单
tools/    前端构建、CSS 资源提取、离线打包、远端运维脚本
```

`data/` 由 `include_bytes!` / `include_str!` 在编译期嵌入二进制，属于构建必需品。

## 许可

MIT。上游项目、ModelTrace 数据与算法、前端第三方库的版权与许可声明，见 [NOTICE.md](NOTICE.md)。

判定结果（是否“降智”、模型指纹、模型归因）均为统计性参考，不构成对模型身份或服务质量的证明。
