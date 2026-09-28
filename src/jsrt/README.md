# jsrt — Tier 2 JS 插件引擎（阶段3）

## 结构
- mod.rs：JsEngine 抽象 trait + QuickJS(rquickjs) 默认后端 + 插件扫描/实例管理
- shim/：注入 QuickJS 的 JS 源码（Yunzai API shim 全家桶）
- bridge.rs：Rust 侧桥接 op（fs/yaml/http/redis/exec/log）

## 设计要点
1. 桥函数全部为**同步语义**（QuickJS 单线程 + block_in_place）：
   - fs 同步 API（readdirSync/readFileSync/writeFileSync/existsSync/mkdirSync/appendFileSync/unlinkSync）
   - `await axios.get()` 中的 await 对同步值透明（≈ JS await 非 Promise）
   - redis get/set/del/keys/incrBy/expire
2. 插件路径映射：`../../../lib/plugins/plugin.js` → 内置 shim 模块
   `../../../lib/common/common.js` → 内置 common shim
3. 超时上下文：shim 版 plugin.js 的 setContext 交由 loader 惰性超时检查（Rust 侧已实现）
4. 生态验收目标：
   - 原版 plugins/example、plugins/system、plugins/other 全部跑通
   - kkp-plugin（axios/fs/yaml/child_process 依赖面）
5. puppeteer 依赖的 app：import 失败 → 跳过该 app（与原版容错一致），其余正常
