// 页签「概览」——按 ui-keepalive.md §2.1 逐控件复刻：
// 3 卡 + 1 顶部状态胶囊 + 1 满宽主按钮（元素顺序 = 卡片数）。
// 未实现的 cmd 由 store catch 到 error，页面据此显示占位（不白屏）。
import { useCallback, useEffect, useState } from 'react'
import { Card, Row, StatusChip, Chip, ActionButton, LinkPill, Text, Mono, Caption, Divider } from '../components'
import { MASKED_KEY, cssVar } from '../theme/styles'
import { call } from '../ipc'
import { useService, serverUrl } from '../store/service'
import { useKeepAlive } from '../store/keepalive'
import { useTrae } from '../store/trae'
import { useNav } from '../store/nav'
import { toast } from '../store/toast'
import { copyWithToast } from '../utils/clipboard'
import { formatExp } from '../store/trae'
import { regionLabelFull, UNSUPPORTED_NOTE } from '../utils/text'

export function OverviewPage() {
  const running = useService((s) => s.running)
  const busy = useService((s) => s.busy)
  const port = useService((s) => s.port)
  const serviceLoaded = useService((s) => s.loaded)
  const serviceError = useService((s) => s.error)
  const toggle = useService((s) => s.toggle)

  const pending = useKeepAlive((s) => s.pending)
  const checksLoaded = useKeepAlive((s) => !s.loading)

  const openKeepAlive = useNav((s) => s.openKeepAlive)
  const openStylePage = useNav((s) => s.openStylePage)

  const trae = useTrae((s) => s.state)
  const traeError = useTrae((s) => s.error)

  const [apiKey, setApiKey] = useState<string | null>(null)
  const [apiKeyError, setApiKeyError] = useState<string | null>(null)
  const [apiKeyVisible, setApiKeyVisible] = useState(false)
  const [lan, setLan] = useState<string | null>(null)

  const localUrl = serverUrl(port)

  useEffect(() => {
    call<string>('api_key')
      .then((k) => setApiKey(typeof k === 'string' && k.length > 0 ? k : null))
      .catch((e: Error) => setApiKeyError(e.message))
    call<string | string[]>('lan_address')
      .then((v) => {
        const first = Array.isArray(v) ? v[0] : v
        setLan(first && first.trim().length > 0 ? first.trim() : null)
      })
      .catch(() => setLan(null))
  }, [])

  const lanUrl = lan ? `http://${lan}:8787/v1` : null

  const copyKey = useCallback(() => {
    // 值始终现读（§2.1 第 3 卡），复制成功才弹 toast
    call<string>('api_key')
      .then((k) => {
        setApiKey(k)
        void copyWithToast(k, '已复制 API Key')
      })
      .catch((e: Error) => toast(e.message))
  }, [])

  const accounts = trae?.logged ?? []
  const traeAccount = accounts.find((a) => !!trae?.activeUid && a.uid === trae.activeUid) ?? accounts[0] ?? null
  const forwardOn = !!traeAccount && (traeAccount.hasAccessToken ?? !!traeAccount.accessToken)
  const uid = traeAccount?.uid || trae?.activeUid || ''
  const regionId = traeAccount?.regionId || trae?.regionId || 'cn'
  const expiresAt = traeAccount?.expiresAt ?? 0
  const nickname = traeAccount?.nickname?.trim() || ''

  return (
    <>
      {/* 1. 服务状态胶囊（页面第一行，非卡片） */}
      <Row top={0}>
        {busy ? (
          <Chip kind="info">{busy}</Chip>
        ) : running ? (
          <StatusChip tone="success" dotOn>
            服务运行中
          </StatusChip>
        ) : (
          <StatusChip tone="error" dotOn={false}>
            服务已停止
          </StatusChip>
        )}
      </Row>

      {/* 2. 卡片「后台常驻」（SOFT）：状态 + 两个入口，无说明文字 */}
      <Card tone="soft">
        <Row gap={12}>
          <Text fs="cardTitle" bold grow>
            {running ? '后台常驻中' : '服务未启动'}
          </Text>
          {checksLoaded && pending === 0 ? (
            <Chip kind="ok">保活就绪</Chip>
          ) : (
            <Chip kind="warn">{pending} 项待开启</Chip>
          )}
        </Row>
        <Row gap={12}>
          <ActionButton variant="outlined" style={{ flex: 1 }} onClick={openKeepAlive}>
            {pending === 0 ? '保活设置' : `去设置保活（${pending} 项）`}
          </ActionButton>
          <ActionButton variant="outlined" style={{ flex: 1 }} onClick={openStylePage}>
            外观
          </ActionButton>
        </Row>
      </Card>

      {/* 3. 卡片「服务地址 + API Key」（EMPHASIS） */}
      <Card tone="emphasis">
        <Caption>本机地址</Caption>
        <Row gap={12}>
          <Mono bold grow style={{ color: cssVar('primary') }}>
            {serviceError ? '地址读取失败' : localUrl}
          </Mono>
          <LinkPill overContainer onClick={() => void copyWithToast(localUrl, `已复制 ${localUrl}`)}>
            复制
          </LinkPill>
        </Row>

        {lanUrl && (
          <>
            <Caption>局域网地址</Caption>
            <Row gap={12}>
              <Mono grow selectable>{lanUrl}</Mono>
              <LinkPill overContainer onClick={() => void copyWithToast(lanUrl, `已复制 ${lanUrl}`)}>
                复制
              </LinkPill>
            </Row>
          </>
        )}

        <Divider />

        <Caption>API Key</Caption>
        <Row gap={12}>
          {apiKeyVisible ? (
            <Mono grow selectable style={{ color: cssVar('onPrimaryContainer') }}>
              {apiKey ?? (apiKeyError ? `未就绪：${apiKeyError}` : '…')}
            </Mono>
          ) : (
            <Mono grow style={{ color: cssVar('onPrimaryContainer') }}>
              {apiKey || apiKeyError ? MASKED_KEY : '…'}
            </Mono>
          )}
          <LinkPill overContainer onClick={() => setApiKeyVisible((v) => !v)}>
            {apiKeyVisible ? '隐藏' : '显示'}
          </LinkPill>
          <LinkPill overContainer onClick={() => void copyKey()}>
            复制
          </LinkPill>
        </Row>
      </Card>

      {/* 4. 满宽主按钮：busy 时置灰禁用（serviceBusy 守卫在 store 内 finally 复位） */}
      <ActionButton variant="full" disabled={!!busy} state={busy ? 'muted' : undefined} onClick={() => void toggle()}>
        {busy ?? (running ? '停止服务' : '启动服务')}
      </ActionButton>
      {serviceLoaded && serviceError && <Caption>服务状态未就绪：{serviceError}</Caption>}

      {/* 5. 卡片「Trae 账号」（NORMAL） */}
      <Card tone="normal">
        {!traeAccount ? (
          <Text fs="cardTitle" bold>
            未登录 Trae
          </Text>
        ) : (
          <>
            <Row gap={12}>
              <Text fs="cardTitle" bold grow>
                {nickname || 'Trae 账号'}
              </Text>
              {forwardOn ? (
                <StatusChip tone="success" dotOn>
                  转发 已启用
                </StatusChip>
              ) : (
                <StatusChip tone="warning" dotOn={false}>
                  转发 未配置
                </StatusChip>
              )}
            </Row>
            {uid && <Caption>{`UID: ${uid} · ${regionLabelFull(regionId)}`}</Caption>}
            {regionId === 'sg' && <Caption>{UNSUPPORTED_NOTE}</Caption>}
            {expiresAt > 0 && <Caption>{`Token 到期: ${formatExp(expiresAt)}`}</Caption>}
          </>
        )}
        {traeError && <Caption>账号状态未就绪：{traeError}</Caption>}
      </Card>
    </>
  )
}
