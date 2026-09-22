import type { ReadingView } from './reading';
import { RecorderDetails } from './RecorderStatus';

const diagnosticLabels: Record<string, string> = { omitted_by_policy: '部分内容未纳入当前阅读范围', observation_gap: '观察副本有缺口', interrupted: '捕获流提前结束', unknown_event: '未识别的事件', missing_identity: '内容归属未确认', conflicting_identity: '身份存在冲突', capacity: '阅读缓存达到上限' };
export function RunDiagnostics({ reading, epoch, processId }: { reading: ReadingView; epoch: string; processId: number | null }) {
  return <details className="wb-run-diagnostics">
    <summary>诊断详情</summary>
    <p>捕获：{reading.capture === 'ok' ? '模型正文观察正常' : '存在缺口、省略或尚无内容'}。</p>
    <RecorderDetails status={reading.recorderStatus} viewSeq={reading.viewSeq} />
    <p>原生记录：{reading.userCapture.enabled ? '后台读取用户提交、命令与文件修改结果，按明确轮次关联' : '尚未启用'}。{reading.userCapture.diagnostics.length ? '存在读取缺口；已有内容仍可阅读。' : ''}</p>
    <dl><dt>运行</dt><dd>{epoch}</dd><dt>CLI 进程</dt><dd>{processId ?? '未接入'}</dd></dl>
    <ul>{reading.diagnostics.map(value => <li key={`${value.requestId}:${value.code}`}>{diagnosticLabels[value.code] || '捕获不完整'} · 请求 {value.requestId.slice(0, 8)}</li>)}</ul>
    <ul>{reading.userCapture.diagnostics.map(value => <li key={`${value.sourceRef}:${value.code}`}>原生来源：{({ invalid_line: '记录无法解析', partial_line: '退出时记录尾部不完整', line_too_large: '记录超过大小限制', missing_identity: '缺少内容身份', identity_conflict: '身份或内容冲突', source_changed: '来源文件发生变化', read_failed: '读取失败', capacity: '原生记录缓存达到上限', unsupported_tool_evidence: '未识别的工具记录' } as Record<string, string>)[value.code] || '观察缺口'} · 来源 {value.sourceRef.slice(0, 8)} / {value.byteOffset}</li>)}</ul>
  </details>;
}
