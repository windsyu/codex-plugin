import type { NativeCommand, NativeFileChange, ToolCall, ToolCategory, ToolContext } from './toolTypes';
import { FileLink, ArgumentFileLink } from './WorkspaceLinks';
import { ProposedDiff } from './ProposedDiff';

const categories: Record<ToolCategory, string> = { command: '命令调用', code: '代码工具', patch: '拟议修改', other: '工具调用' };
const argumentsLabel = { receiving: '参数生成中', generated: '参数已生成', incomplete: '参数不完整' };
const executionLabel = { unobserved: '尚未观察到执行', running: '执行中', result_observed: '结果已观察 · 执行状态未确认', succeeded: '执行成功', failed: '执行失败', declined: '已拒绝执行' };

export function ToolCard({ tool, historical = false, onInspect }: { tool: ToolCall; historical?: boolean; onInspect?: () => void }) {
  const conflict = tool.identityConflict || tool.resultConflict;
  return <article className={`wb-tool-card wb-tool-${tool.category}`} data-kind="tool_call" data-category={tool.category} data-call-id={tool.callId || ''} data-execution={conflict ? 'unobserved' : tool.execution}>
    <div className="wb-tool-heading"><span aria-hidden="true">{tool.category === 'command' ? '›_' : tool.category === 'code' ? '{ }' : '◇'}</span><strong>{categories[tool.category] || '工具调用'}</strong><code>{tool.namespace ? `${tool.namespace}.` : ''}{tool.name || '名称未确认'}</code>{onInspect && <button className="wb-inspect-link" onClick={onInspect}>调用详情</button>}</div>
    <div className="wb-tool-states">{historical && <span>保存时：</span>}<span>{historical && tool.argumentsState === 'receiving' ? '参数尚未完整生成' : argumentsLabel[tool.argumentsState]}</span><span className={`wb-execution ${conflict ? '' : tool.execution}`}>{conflict ? '关联存在冲突 · 执行状态未确认' : historical && tool.execution === 'running' ? '尚未观察到执行结束' : executionLabel[tool.execution]}</span></div>
    {tool.command ? <><pre className="wb-command-preview">{tool.command.text}</pre><p className="wb-subtle">工作目录：{tool.command.cwd || '未捕获'}</p></>
      : tool.proposedPatch?.state !== 'ready' && <pre className="wb-tool-preview">{tool.arguments.slice(0, 240) || (historical ? '未保存可展示的参数' : '等待可展示的参数')}{tool.arguments.length > 240 ? '…' : ''}</pre>}
    {(!tool.namespace || tool.namespace === 'functions') && ['read_file', 'view_file', 'open_file'].includes(tool.name || '') && <ArgumentFileLink arguments={tool.arguments} cwd={tool.command?.cwd} />}
    {tool.category === 'code' && <p className="wb-subtle">代码工具中的子调用以实际执行证据为准。</p>}
    {tool.category === 'patch' && <p className="wb-subtle">模型提出的修改内容；不代表工作区已修改。</p>}
    {tool.category === 'patch' && tool.proposedPatch && <ProposedDiff patch={tool.proposedPatch} />}
    {!tool.callId && <p className="wb-notice">调用 ID 未确认，结果暂无法关联。</p>}
    {conflict && <p className="wb-notice">保留已有证据；可在“调用详情”查看请求中的不同结果，不能据此确认执行成功。</p>}
    <details className="wb-tool-arguments" key="arguments"><summary>查看捕获参数</summary><pre>{tool.arguments || '没有可展示的参数'}</pre>{tool.truncated && <p className="wb-subtle">参数预览已截断，完整参数未保存在工作台。</p>}</details>
    {tool.result && <section className="wb-tool-result" aria-label="工具结果">
      <div className="wb-tool-result-meta"><strong>已捕获结果</strong>{tool.category === 'command' && <span>退出码：{tool.result.exitCode ?? '未捕获'} · 耗时：{tool.result.durationMs === null ? '未捕获' : `${Math.round(tool.result.durationMs * 10) / 10} ms`}</span>}</div>
      <p className="wb-subtle">来源：{tool.result.source.kind === 'model_request' ? `后续模型请求 ${tool.result.source.requestId.slice(0, 8)} / 输入项 ${tool.result.source.inputIndex}` : `原生运行记录 · 调用 ${tool.result.source.nativeItemId}`}{!tool.result.streams && ' · 输出未区分 stdout / stderr'}</p>
      <details open={tool.execution === 'failed' || tool.execution === 'declined'}><summary>查看输出</summary>{tool.result.streams ? <OutputStreams streams={tool.result.streams} /> : <pre>{tool.result.output || '没有可展示的文本输出'}</pre>}</details>
      {tool.result.truncated && <p className="wb-notice">输出预览超限或来源含截断标记，完整日志未保存在工作台。</p>}
      {tool.result.omitted && <p className="wb-notice">部分非文本结果已按策略省略。</p>}
    </section>}
    <div className="wb-message-status">模型响应中的调用 · 请求 {tool.key.requestId.slice(0, 8)} / 观察 {tool.captureSeq}</div>
  </article>;
}

function OutputStreams({ streams }: { streams: { stdout: string | null; stderr: string | null } }) {
  return <div className="wb-output-streams">{(['stdout', 'stderr'] as const).map(name => <section key={name} aria-label={name}><p className="wb-subtle">{name === 'stdout' ? '标准输出 stdout' : '标准错误 stderr'}</p><pre>{streams[name] ?? '未捕获'}{streams[name] === '' ? '空输出' : ''}</pre></section>)}</div>;
}

export function NativeFileChangeDetails({ changes }: { changes: NativeFileChange[] }) {
  if (!changes.length) return null;
  return <details className="wb-native-commands wb-native-file-changes"><summary>同一轮的原生文件修改记录（{changes.length}）</summary>
    <p className="wb-subtle">来自 CLI 运行记录；仅明确匹配的调用会补到工具卡。这里的路径是原生报告的修改范围，当前工作区 Diff 可与它不同。</p>
    {changes.map(change => <section key={`${change.source.sourceRef}:${change.source.byteOffset}`}>
      <p>调用 {change.key.nativeItemId} · {({ completed: '执行成功', failed: '执行失败', declined: '已拒绝执行' })[change.status]}</p>
      <ul>{change.files.map((file, index) => <li key={index}>{({ add: '新增', update: '修改', delete: '删除', unknown: '未知修改' })[file.operation]} <FileLink path={file.path} />{file.moveTo && <> → <FileLink path={file.moveTo} /></>}</li>)}</ul>
      <OutputStreams streams={change} />
      <p className="wb-subtle">原生记录 {change.source.sourceRef.slice(0, 8)} / {change.source.byteOffset}</p>
      {(change.truncated || change.omitted) && <p className="wb-notice">记录存在截断或省略，完整日志未保存在工作台。</p>}
    </section>)}
  </details>;
}

export function ToolContextDetails({ context }: { context: ToolContext }) {
  return <div className="wb-tool-context">
    {!!context.definitions.length && <details><summary>工具定义（{context.definitions.length}）</summary><ul>{context.definitions.map((definition, index) => <li key={index}><code>{definition.namespace ? `${definition.namespace}.` : ''}{definition.name}</code> · {definition.toolKind} · 来源 {definition.source}{definition.inputIndex === null ? '' : ` / 输入项 ${definition.inputIndex}`}</li>)}</ul></details>}
    {!!context.outputs.length && <details><summary>请求携带的工具结果（{context.outputs.length}）</summary><p className="wb-subtle">这里包含历史上下文。执行状态以工具卡的明确关联为准。</p>{context.outputs.map(output => <section key={output.inputIndex}><p>调用 {output.callId || 'ID 未确认'} · 输入项 {output.inputIndex}</p><pre>{output.output || '没有可展示的文本'}</pre>{(output.omitted || output.truncated) && <p className="wb-notice">结果存在省略或截断。</p>}</section>)}</details>}
    {context.partial && <p className="wb-notice">工具上下文存在未知类型或超限省略。</p>}
  </div>;
}

export function NativeCommandDetails({ commands, historical = false }: { commands: NativeCommand[]; historical?: boolean }) {
  if (!commands.length) return null;
  return <details className="wb-native-commands"><summary>同一轮的原生执行记录（{commands.length}）</summary>
    <p className="wb-subtle">来自 CLI 运行记录；只有明确匹配的调用会补到工具卡。代码工具内部的命令不按名称或代码内容猜测归属。</p>
    {commands.map(command => <section key={`${command.source.sourceRef}:${command.source.byteOffset}`}>
      <p>调用 {command.key.nativeItemId} · {historical && '保存时：'}{historical && command.status === 'in_progress' ? '尚未观察到执行结束' : ({ in_progress: '执行中', completed: '已完成', failed: '执行失败', declined: '已拒绝执行' })[command.status]} · 退出码：{command.exitCode ?? '未捕获'}</p>
      <pre>{JSON.stringify(command.command)}</pre><p className="wb-subtle">工作目录：{command.cwd || '未捕获'} · 输出未区分 stdout / stderr</p>
      <pre>{command.output || '没有可展示的文本输出'}</pre>
      <p className="wb-subtle">原生记录 {command.source.sourceRef.slice(0, 8)} / {command.source.byteOffset}{command.processId ? ` · 进程 ${command.processId}` : ''}</p>
      {(command.truncated || command.omitted) && <p className="wb-notice">记录存在截断或省略，完整日志未保存在工作台。</p>}
    </section>)}
  </details>;
}
