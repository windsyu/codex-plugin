import { FileLink } from './WorkspaceLinks';
import type { ProposedPatch } from './toolTypes';

const operations = { add: '新增', update: '修改', delete: '删除' };
const reasons = {
  incomplete: '参数尚不完整，无法显示完整 Diff。',
  truncated: '捕获参数已截断，无法显示完整 Diff。',
  identity_conflict: '调用身份存在冲突，无法确认这份 Diff。',
  unsupported_format: '未识别此修改格式，可展开捕获参数阅读。',
  budget: '修改超出结构化预览上限，可展开捕获参数阅读。',
};
const marks = { added: '+', removed: '−', context: ' ' };

export function ProposedDiff({ patch }: { patch: ProposedPatch }) {
  if (patch.state === 'unavailable') return <p className="wb-notice" role="status">{reasons[patch.reason]}</p>;
  return <section className="wb-proposed-diff" aria-label="模型拟议 Diff">
    <div className="wb-diff-heading"><strong>拟议 Diff</strong><span>{patch.files.length} 个文件</span></div>
    {patch.environmentId && <p className="wb-subtle">目标环境：{patch.environmentId}</p>}
    {!patch.files.length && <p className="wb-subtle">参数中没有文件修改。</p>}
    {patch.files.map((file, index) => <details className="wb-diff-file" key={index} open={index === 0}>
      <summary><span className="wb-diff-operation">{file.moveTo ? '移动并修改' : operations[file.operation]}</span><code>{file.path}{file.moveTo && <> → {file.moveTo}</>}</code><span className="wb-diff-counts"><span>+{file.addedLines}</span><span>−{file.removedLines ?? '?'}</span></span></summary>
      <p className="wb-tool-path">查看当前文件：<FileLink path={file.moveTo || file.path} remote={!!patch.environmentId} /></p>
      {file.operation === 'delete' ? <p className="wb-subtle">拟议删除整文件；参数未包含原内容，删除行数未知。</p>
        : !file.sections.length ? <p className="wb-subtle">拟议新增空文件。</p>
          : <div className="wb-diff-lines" tabIndex={0} role="region" aria-label={`${file.path} 的拟议增删行`}>
            {file.sections.map((section, sectionIndex) => <div className="wb-diff-section" key={sectionIndex}>
              <div className="wb-diff-anchor">{section.anchor ? `@@ ${section.anchor}` : `片段 ${sectionIndex + 1}`}</div>
              {section.lines.map((line, lineIndex) => <div className={`wb-diff-line ${line.kind}`} key={lineIndex}><span className="wb-diff-mark" aria-label={line.kind === 'added' ? '新增行' : line.kind === 'removed' ? '删除行' : '上下文行'}>{marks[line.kind]}</span><code>{line.text || '\u00a0'}</code></div>)}
              {section.atEof && <div className="wb-diff-anchor">文件末尾</div>}
            </div>)}
          </div>}
    </details>)}
    {!!patch.files.length && <p className="wb-subtle">按工具参数展示增删行；文件中的实际行号未捕获。</p>}
  </section>;
}
