// 二级页「保活设置」（桌面等值：常驻体检）——按 ui-keepalive.md §2.5 复刻 4 卡 + 2 section 结构，
// 检查项数据来自 desktop_checks，开关项写回 config_update。
import { useEffect } from 'react'
import { Card, SectionTitle, Row, Chip, ActionButton, Text, Caption, Divider } from '../components'
import { useKeepAlive } from '../store/keepalive'
import { useSettings } from '../store/settings'
import { cssVar } from '../theme/styles'

export function KeepAlivePage() {
  const checks = useKeepAlive((s) => s.checks)
  const pending = useKeepAlive((s) => s.pending)
  const autoFixablePending = useKeepAlive((s) => s.autoFixablePending)
  const loaded = useKeepAlive((s) => s.loaded)
  const error = useKeepAlive((s) => s.error)
  const load = useKeepAlive((s) => s.load)
  const autoConfigureOne = useKeepAlive((s) => s.autoConfigureOne)
  const fixItem = useKeepAlive((s) => s.fixItem)
  const setCloseToTray = useKeepAlive((s) => s.setCloseToTray)
  const setAutostart = useKeepAlive((s) => s.setAutostart)

  const config = useSettings((s) => s.config)

  useEffect(() => {
    void load()
  }, [load])

  const ready = loaded && pending === 0

  return (
    <>
      {/* 1. 总状态卡 */}
      <Card tone="normal" top={false}>
        <Row gap={12}>
          <Text fs="cardTitle" bold grow>
            {ready ? '保活已就绪' : `还有 ${pending} 项待开启`}
          </Text>
          {ready ? <Chip kind="ok">{'✓ 全部正常'}</Chip> : <Chip kind="warn">{'⚠ 需要处理'}</Chip>}
        </Row>
        <Caption>
          {ready
            ? '网关已能在后台长期运行，切到别的 App、息屏都不会断。'
            : '下面标 ⚠ 的项会让服务在真机上被系统清掉，建议逐项开启。'}
        </Caption>
        {error && <Caption>体检接口未就绪：{error}</Caption>}
      </Card>

      {/* 2. 一键配置卡 */}
      <Card tone="normal">
        <Text fs="cardTitle" bold>
          一键配置
        </Text>
        <Caption>
          {autoFixablePending === 0
            ? '能自动跳转的项都已就绪。'
            : `有 ${autoFixablePending} 项可以自动打开系统页面，你点「允许」即可。`}
        </Caption>
        <ActionButton variant="full" onClick={() => void autoConfigureOne()}>
          {`开始自动配置${autoFixablePending > 0 ? `（${autoFixablePending} 项）` : ''}`}
        </ActionButton>
      </Card>

      {/* 3. 逐项检查 */}
      <SectionTitle>逐项检查</SectionTitle>
      <Card tone="normal">
        {checks.length === 0 && (
          <Caption>{loaded ? '暂无体检项（后端 desktop_checks 还没返回数据）。' : '正在读取体检项…'}</Caption>
        )}
        {checks.map((c, i) => (
          <div key={c.title} className="tw-stack">
            {i > 0 && <Divider />}
            <Row gap={12}>
              <Text fs="value" bold style={{ color: cssVar(c.ok ? 'success' : 'warning') }}>
                {c.ok ? '✓' : '⚠'}
              </Text>
              <Text fs="value" bold grow>
                {c.title}
              </Text>
              {!c.ok && (c.autoFixable ? <Chip kind="info">可一键</Chip> : <Chip kind="warn">需手动</Chip>)}
            </Row>
            {!c.ok && <Caption>{c.why}</Caption>}
            {!c.ok && c.autoFixable && (
              <Row gap={12}>
                <ActionButton variant="outlined" onClick={() => void fixItem(c)}>
                  去开启
                </ActionButton>
              </Row>
            )}
          </div>
        ))}
      </Card>

      {/* 4. 系统开关（替代 Android 的 ROM 手动确认卡：桌面侧可查可改） */}
      <SectionTitle>本机需要确认的项</SectionTitle>
      <Card tone="normal">
        <Row gap={12}>
          <Text fs="body" bold grow>
            关窗最小化到托盘
          </Text>
          <ActionButton
            variant="outlined"
            state={config.closeToTray ? 'done' : 'muted'}
            onClick={() => void setCloseToTray(!config.closeToTray)}
          >
            {config.closeToTray ? '已开启' : '已关闭'}
          </ActionButton>
        </Row>
        <Caption>
          {config.closeToTray
            ? '点窗口右上角 × 后应用缩到托盘，后台网关继续运行。'
            : '点窗口右上角 × 会直接退出应用，后台网关随之停止。'}
        </Caption>
        <Divider />
        <Row gap={12}>
          <Text fs="body" bold grow>
            开机自启
          </Text>
          <ActionButton
            variant="outlined"
            state={config.autostart ? 'done' : 'muted'}
            onClick={() => void setAutostart(!config.autostart)}
          >
            {config.autostart ? '已开启' : '已关闭'}
          </ActionButton>
        </Row>
        <Caption>系统不提供统一查询接口，开关以实际写回结果为准（默认保持关闭）。</Caption>
      </Card>
    </>
  )
}
