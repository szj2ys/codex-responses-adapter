# PRD: Codex Responses Adapter 改进

## Problem Statement

codex-responses-adapter 目前存在以下问题：

1. **配置语义混淆**：`models.routes` 是数组，但实际代码实现了 ordered fallback 逻辑，这与用户的预期不符。用户配置多个 route 时，期望的是优先级 fallback，但实际需求是单一路由。

2. **错误处理不精确**：所有 upstream 错误（包括 4xx 客户端错误）都触发 fallback，这可能导致：
   - 用户输入超长导致 400，却 fallback 到另一个 provider 产生意外账单
   - API key 无效导致 401，也静默 fallback

3. **测试缺口**：项目没有端到端测试验证 Codex CLI 能正确解析 adapter 的响应。SSE 事件格式、tool call ID 的 round-trip、usage 字段的兼容性都没有被验证。

4. **Usage 字段兼容性问题**：如果 upstream provider 不返回 usage，adapter 会发送 `usage: null`，Codex CLI 可能无法正确处理。

## Solution

1. **破坏性变更**：将 `models.routes` 改为 `models.route`，类型从 `Vec<RouteTarget>` 改为单个 `RouteTarget`，彻底移除 fallback 概念。

2. **精确错误处理**：区分网络错误（可重试）和 HTTP 错误（不可重试）。4xx 错误直接返回给客户端，不再尝试其他操作。

3. **添加 Mock Codex Client 测试**：创建一个模拟 Codex CLI 的测试客户端，验证：
   - SSE 事件序列能被正确解析
   - Tool call ID 的 round-trip 一致性
   - Usage 字段缺失时的行为

4. **Usage 字段处理**：当 upstream 不返回 usage 时，omit 整个 usage 字段而不是发送 null。

## User Stories

1. 作为一个 adapter 用户，我希望配置中只定义单个 route，以避免对 fallback 行为的困惑。

2. 作为一个 adapter 用户，当我的请求参数有误时，我希望立即收到错误提示，而不是被静默转发到其他 provider。

3. 作为一个开发者，我希望有自动化测试验证 Codex CLI 能正确解析 adapter 的响应，以避免回归问题。

4. 作为一个 adapter 用户，我希望即使 upstream provider 不返回 token usage，adapter 也能正常工作。

5. 作为一个维护者，我希望代码库的配置语义清晰明确，减少用户误解的可能性。

6. 作为一个 MiniMax 用户，我希望 system message 能被正确合并，以避免 provider 返回错误。

7. 作为一个 streaming 用户，我希望 tool call 在 streaming 模式下的行为与 non-streaming 一致。

8. 作为一个开发者，我希望 tool ID 的生成逻辑在所有代码路径中保持一致。

## Implementation Decisions

### Module: Config

**变更**：`ModelEntry.routes: Vec<RouteTarget>` → `ModelEntry.route: RouteTarget`

**理由**：
- 消除 ordered fallback 的歧义
- 简化 handler 中的路由逻辑（移除 for 循环）

**迁移路径**：
- 启动时检查旧配置格式，报错提示用户迁移
- 更新 example.toml 和文档

### Module: Handler

**变更**：移除 `routes.iter().enumerate()` 循环，直接使用单个 route

**错误处理策略**：
```rust
match send_chat_request(...).await {
    Ok(resp) => resp,
    Err(AdapterError::TransportError(_)) => {
        // 网络错误，返回 502
        return adapter_error_response(...);
    }
    Err(AdapterError::UpstreamError { status, body }) if status >= 500 => {
        // 5xx 错误，返回 502
        return adapter_error_response(...);
    }
    Err(e) => {
        // 4xx 或其他错误，透传给客户端
        return adapter_error_response(e);
    }
}
```

### Module: Response Converter

**变更**：`response.completed` 事件中的 usage 处理

```rust
let completed = json!({
    "type": "response.completed",
    "response": {
        "id": response_id,
        // 只有当 usage 存在时才包含该字段
        "usage": usage_json,  // 或完全 omit
    }
});
```

### Module: Testing

**新增**：`tests/integration/codex_client_mock.rs`

**职责**：
- 模拟 Codex CLI 的 SSE 解析逻辑
- 验证事件序列：`response.created` → `response.output_item.done` → `response.completed`
- 验证 tool call round-trip：发送 `FunctionCallOutput` 时使用的 `call_id` 与之前收到的一致

**测试场景**：
1. 简单文本请求
2. Tool call 完整流程
3. Streaming 模式
4. Usage 缺失的情况

## Testing Decisions

### 测试哲学

只测试外部行为，不测试实现细节。测试应该验证「Codex CLI 能正常工作」，而不是「代码执行了某行」。

### 测试模块

1. **Config 解析测试**：验证新旧配置格式的解析和报错
2. **Mock Codex Client 集成测试**：验证端到端流程
3. **Response Converter 测试**：验证 SSE 事件格式

### Prior Art

- `response_converter.rs` 已有单元测试（`#[cfg(test)]` 模块）
- `request_converter.rs` 已有单元测试
- 参考 Codex CLI 的 `responsesProxy.ts` 测试工具

## Out of Scope

1. 不添加真实的 Codex CLI 作为测试依赖（太重）
2. 不改变 web_search 的策略逻辑（`PreferPassthrough` 保持当前语义）
3. 不添加新的 provider 类型
4. 不实现 session state 管理（`previous_response_id`）

## Further Notes

### Breaking Change 通知

由于这是破坏性变更，需要在：
1. README 中添加 migration guide
2. 启动时检测旧配置并给出清晰的错误信息
3. GitHub release notes 中标注 breaking change

### Tool ID 一致性

基于 Codex 源码验证，`function_call` 的 `call_id` 字段是必须的（非空字符串）。当前的 `normalize_tool_id` 逻辑是正确的：
- 如果 upstream 返回空 ID，生成 `call_{uuid}`
- 否则保留原 ID

### 后续改进

1. 考虑添加配置热重载（hot reload）
2. 考虑添加 metrics 和 health check 端点
3. 考虑实现 `previous_response_id` 的内存缓存
