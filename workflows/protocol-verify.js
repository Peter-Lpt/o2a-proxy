// o2a-proxy 协议转换专项验证 workflow
// 用法: workflow({ name: 'protocol-verify', args: { focus?, runTests? } })
// focus: 可选重点（anthropic / responses / passthrough / direct / usage，默认全量静态审计）；
// runTests: true 时先跑 test_codex_direct.py 端到端（默认 false，只做静态审计，速度更快）。
export const meta = {
  name: 'protocol_verify',
  description: 'o2a-proxy 协议矩阵专项验证：三协议（Anthropic/Chat/Responses）转换与 SSE 时序静态审计 + 可选端到端测试',
  phases: [
    { title: 'Matrix Audit' },
    { title: 'E2E' },
    { title: 'Report' },
  ],
}

const CTX = [
  'o2a-proxy 协议矩阵（README 权威口径）：',
  '1. api=anthropic-messages：Anthropic 入口。账号有 anthropic 端点 → direct 透传；只有 openai 端点 → convert_request 转 Chat 发送（claude 模式）。',
  '2. api=openai-completions：Chat 入口 → 整包透传，仅缺 model 时注入服务配置。',
  '3. api=openai-responses + upstream_api=openai-responses：Responses 整包透传（上游原生支持，如 DeepSeek 官方 /v1/responses，零转换）。',
  '4. api=openai-responses（默认 upstream=chat）：Responses → Chat 转换发送，响应转回 Responses 事件。',
  '转换函数：proxy.py 的 convert_request（Anthropic→Chat）、_responses_to_chat（Responses→Chat）、_chat_to_responses_json / _ResponsesStreamTranslator（Chat→Responses）；',
  '引擎：proxy_async.py 的 handle_claude_stream/non_stream、handle_openai_stream/non_stream、handle_direct_stream/non_stream、handle_passthrough。',
  'SSE 关键事件：Anthropic message_start/content_block_start/delta/stop、message_delta（stop_reason+usage）、message_stop；',
  'Responses response.created / response.output_text.delta / response.function_call_arguments.delta / response.completed；Chat chunk + [DONE]。',
  'usage 缓存字段：cache_creation_input_tokens / cache_read_input_tokens 三协议映射。',
  '端到端测试：test_codex_direct.py（mock 上游 18901 + 真实引擎 18902，覆盖形态 1/3/4 中的 chat 透传、responses 透传、responses→chat）。',
].join('\n')

const focus = (args && args.focus) || ''
const runTests = !!(args && args.runTests)

phase('Matrix Audit')
const audits = await parallel([
  () => agent(
    'You are the protocol auditor for o2a-proxy. Statically audit the Anthropic→Chat conversion path: ' +
    'convert_request (messages/roles/content blocks/tool_use/tool_result/system/max_tokens/tools/tool_choice), ' +
    'and the SSE streaming translation in handle_claude_stream (event sequence, content block indexes, thinking blocks, ' +
    'tool_use input_json assembly, usage cache fields, stop_reason mapping, timeout closure). ' +
    'Verify against README matrix rows 1 (and 4 for anthropic-entry cases). Cite file:line for every finding. ' +
    'Output: VERIFIED items, ISSUES (file:line + scenario), RISKS.\n\n项目背景：\n' + CTX + (focus && focus !== 'anthropic' ? '\n\n(当前重点仅审计 Anthropic 路径)' : ''),
    { label: 'audit-anthropic', agentType: 'protocol-auditor' }
  ),
  () => agent(
    'You are the protocol auditor for o2a-proxy. Statically audit the Responses↔Chat path: ' +
    '_responses_to_chat (input items: message/function_call/reasoning; store; reasoning effort), ' +
    '_chat_to_responses_json and _ResponsesStreamTranslator (response.created / output_text.delta / ' +
    'function_call_arguments.delta / output index continuity / item_id / response.completed with usage), ' +
    'and handle_openai_stream / handle_openai_non_stream (upstream_api=chat conversion vs passthrough). ' +
    'Verify against README matrix rows 3 and 4. Cite file:line for every finding. ' +
    'Output: VERIFIED items, ISSUES (file:line + scenario), RISKS.\n\n项目背景：\n' + CTX + (focus && focus !== 'responses' ? '\n\n(当前重点仅审计 Responses 路径)' : ''),
    { label: 'audit-responses', agentType: 'protocol-auditor' }
  ),
  () => agent(
    'You are the protocol auditor for o2a-proxy. Statically audit the passthrough & direct paths: ' +
    'handle_passthrough (chat 整包透传，仅注入缺失 model) and handle_direct_stream/non_stream + ' +
    'upstream_direct_headers (Anthropic 原生透传：x-api-key / anthropic-version / body 透传，不转换)。 ' +
    'Check error propagation for non-200 upstream (status code + body preserved; SSE error events). ' +
    'Also check Service.mode derivation matches README matrix. Cite file:line. ' +
    'Output: VERIFIED items, ISSUES (file:line + scenario), RISKS.\n\n项目背景：\n' + CTX,
    { label: 'audit-passthrough', agentType: 'protocol-auditor' }
  ),
])

phase('E2E')
const e2e = runTests
  ? await agent(
      'You are the tester for o2a-proxy. Run the end-to-end protocol test and report with evidence ' +
      '(exit code + key output): python -m pytest test_codex_direct.py -v. ' +
      'If ports 18901/18902 are occupied, report it as an environment blocker (do not modify the test). ' +
      'If failures occur, quote the failing assertion and map it to the conversion path.\n\n项目背景：\n' + CTX,
      { label: 'e2e', agentType: 'tester' }
    )
  : 'E2E skipped (args.runTests not set; pass runTests: true to execute test_codex_direct.py).'

phase('Report')
const report = await agent(
  'You are the protocol lead for o2a-proxy. Produce the protocol matrix verification report: for each matrix row ' +
  '(anthropic-messages claude/direct、openai-completions、openai-responses × upstream) state VERIFIED / AT-RISK / BROKEN ' +
  'with evidence; list every ISSUE from the audits with file:line and severity; recommend fixes; one-line verdict.\n\n' +
  'AUDITS:\n' + JSON.stringify(audits) + '\n\nE2E:\n' + e2e,
  { label: 'protocol-report', agentType: 'protocol-auditor' }
)

return { audits, e2e, report }
