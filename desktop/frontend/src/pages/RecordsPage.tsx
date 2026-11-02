// 页签「记录」——按 ui-keepalive.md §2.4 复刻：汇总卡（每组占比条对齐最大组）+ 逐记录卡 + 空态卡。
import { useEffect, useMemo } from 'react'
import { Card, SectionTitle, Row, StatusChip, Text, Mono, Caption, ShareBar } from '../components'
import { useRecords, type ModelAgg, type UsageRecord } from '../store/records'
import { hms } from '../utils/text'

/// UsageStore.aggregate 等值：按 model 分组求和（后端未回 agg 时本地聚合，保持汇总卡语义一致）
function aggregate(items: UsageRecord[]): ModelAgg[] {
  const map = new Map<string, ModelAgg>()
  for (const r of items) {
    const key = r.model || '?'
    const cur = map.get(key) ?? { model: key, calls: 0, okCalls: 0, prompt: 0, completion: 0, total: 0, ms: 0 }
    cur.calls += 1
    if (r.ok) cur.okCalls += 1
    cur.prompt += r.prompt
    cur.completion += r.completion
    cur.total += r.total
    cur.ms += r.ms
    map.set(key, cur)
  }
  return map.size > 0 ? [...map.values()].sort((a, b) => b.total - a.total) : []
}

export function RecordsPage() {
  const data = useRecords((s) => s.data)
  const error = useRecords((s) => s.error)
  const load = useRecords((s) => s.load)

  useEffect(() => {
    void load()
  }, [load])

  const items = useMemo(() => data?.items ?? [], [data])
  const agg = useMemo(() => {
    const fromServer = data?.agg
    if (Array.isArray(fromServer) && fromServer.length > 0) return fromServer
    return aggregate(items)
  }, [data, items])
  const maxTotal = agg.reduce((acc, g) => Math.max(acc, g.total), 0)

  if (items.length === 0) {
    return (
      <>
        <SectionTitle>消耗汇总</SectionTitle>
        <Card tone="soft">
          <Caption>暂无记录 —— 发一次聊天请求后这里会显示用量与汇总</Caption>
          {error && <Caption>记录接口未就绪：{error}</Caption>}
        </Card>
      </>
    )
  }

  return (
    <>
      <Card tone="normal">
        <Text fs="cardTitle" bold>
          模型消耗汇总
        </Text>
        {agg.map((g) => (
          <div key={g.model} className="tw-stack">
            <Row gap={12} top={12}>
              <Text fs="value" bold grow maxLines={1}>
                {g.model || '?'}
              </Text>
              <Mono fs="caption" color="onSurfaceVariant">
                {`总 ${g.total} · 输出 ${g.completion}`}
              </Mono>
            </Row>
            <ShareBar value={g.total} max={maxTotal} />
          </div>
        ))}
      </Card>

      <SectionTitle>{`调用记录（最近 ${items.length} 条）`}</SectionTitle>

      {[...items].reverse().map((r, i) => (
        <Card key={`${r.ts}-${i}`} tone="normal">
          <Row gap={12}>
            {r.ok ? (
              <StatusChip tone="success" dotOn>
                OK
              </StatusChip>
            ) : (
              <StatusChip tone="error" dotOn={false}>
                ERR
              </StatusChip>
            )}
            <Text fs="body" grow maxLines={1}>
              {r.model || '?'}
            </Text>
            <Mono fs="chip" color="onSurfaceVariant">
              {hms(r.ts)}
            </Mono>
          </Row>
          <Caption>{`in ${r.prompt} · out ${r.completion} · 总 ${r.total} · ${r.ms}ms${r.err ? ` · ${r.err}` : ''}`}</Caption>
        </Card>
      ))}
    </>
  )
}
