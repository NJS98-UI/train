// 页签「账号」完整复刻（ui-keepalive.md §2.2）：积分总计 / TRAE 账号 / Qoder 账号 /
// 聊天转发后端 / 自动换号 / 一键签到 + 账户选择条与弹窗。
import { useEffect, useState } from 'react'
import {
  AccountBar,
  ActionButton,
  Card,
  Caption,
  Chip,
  ConfirmDialog,
  DiagnoseDialog,
  LabelValue,
  LinkPill,
  PickerDialog,
  Row,
  SectionTitle,
  Text,
  TimePickerDialog,
} from '../components'
import type { PickerRow } from '../components'
import {

  activeTraeAccount,
  traeLoginStart,
  traeLoginSubmit,
  traeRemove,
  traeSwitch,
  useTrae,
} from '../store/trae'
import type { TraeAccountView } from '../store/trae'
import { qoderFingerprintGet, qoderFingerprintSet, qoderLoginGuide, qoderLoginPat, qoderModeLabel, qoderRemove, qoderSwitch, useQoder } from '../store/qoder'
import { useCredits } from '../store/credits'
import { scheduleGet, scheduleSet, useAutoSwitch, useBackend, backupExport, backupImport } from '../store/credits'
import { toast } from '../store/toast'

function creditValue(v: number | null | undefined): string {
  if (v === null || v === undefined) return '—'
  return Number.isInteger(v) ? String(v) : v.toFixed(1)
}

function creditUpdated(snapshot: ReturnType<typeof useCredits.getState>['snapshot']): string {
  if (snapshot?.loading) return '正在查询全部账号…'
  if (!snapshot?.at) return '尚未查询（点右上「刷新积分」）'
  const d = new Date(snapshot.at)
  const p = (n: number) => String(n).padStart(2, '0')
  return `更新于 ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

function tokenRenewLabel(account: TraeAccountView | null, authFailedUid?: string | null, now = Date.now()): string {
  if (account?.uid && authFailedUid === account.uid) return '需重新登录'
  const exp = account?.expiresAt ?? 0
  if (exp <= 0) return '未知（无到期信息）'
  const ms = exp * 1000 - now
  if (ms <= 0) return '已过期 · 需重新登录'
  if (ms <= 3_600_000) return '临近到期 · 将自动续期'
  return '有效'
}

function regionLabelShort(region?: string): string {
  return region === 'sg' ? '国际服' : '国服'
}

export function AccountPage() {
  const trae = useTrae((s) => s.state)
  const qoder = useQoder((s) => s.state)
  const loadTrae = useTrae((s) => s.load)
  const loadQoder = useQoder((s) => s.load)
  const snap = useCredits((s) => s.snapshot)
  const quotaLoading = useCredits((s) => s.quotaLoading)
  const checkinBusy = useCredits((s) => s.checkinBusy)
  const qoderClaimBusy = useCredits((s) => s.qoderClaimBusy)
  const credits = useCredits.getState()
  const backend = useBackend((s) => s.backend)
  const backendSet = useBackend((s) => s.set)
  const auto = useAutoSwitch()

  const [traePicker, setTraePicker] = useState(false)
  const [qoderPicker, setQoderPicker] = useState(false)
  const [loginUrlOpen, setLoginUrlOpen] = useState(false)
  const [patOpen, setPatOpen] = useState(false)
  const [fpOpen, setFpOpen] = useState(false)
  const [timeOpen, setTimeOpen] = useState<'trae' | 'qoder' | null>(null)
  const [diag, setDiag] = useState<{ title: string; text: string } | null>(null)
  const [cbUrl, setCbUrl] = useState('')
  const [pat, setPat] = useState('')
  const [patMode, setPatMode] = useState('global')
  const [fp, setFp] = useState({ id: '', token: '', machineType: '' })
  const [traeSchedule, setTraeSchedule] = useState({ enabled: false, hhmm: '08:00' })
  const [qoderSchedule, setQoderSchedule] = useState({ enabled: false, hhmm: '09:00' })

  useEffect(() => {
    void loadTrae()
    void loadQoder()
    void credits.load()
    void useAutoSwitch.getState().load()
    void scheduleGet('trae').then((r) => r && setTraeSchedule(r))
    void scheduleGet('qoder').then((r) => r && setQoderSchedule(r))
  }, [loadTrae, loadQoder])

  const activeTrae = activeTraeAccount(trae)
  const activeQoder = qoder?.logged?.find((a) => a.uid && a.uid === qoder?.activeUid) ?? qoder?.logged?.[0] ?? null

  const traeRows: PickerRow[] = (trae?.logged ?? []).map((a) => ({
    key: a.uid ?? '',
    primary: a.nickname || a.uid,
    caption: `${regionLabelShort(a.regionId)} · 积分：${snapItemsValue(snap, 'trae', a.uid)}`,
    current: a.uid === trae?.activeUid,
    onClick: async () => {
      setTraePicker(false)
      if (a.uid && a.uid !== trae?.activeUid) await traeSwitch(a.uid)
    },
    onDelete: a.uid
      ? () => {
          if (confirm(`删除账号「${a.nickname || a.uid}」？本地凭证会一起清掉。`)) void traeRemove(a.uid!, a.nickname ?? a.uid!)
        }
      : undefined,
  }))

  const qoderRows: PickerRow[] = (qoder?.logged ?? []).map((a) => ({
    key: a.uid ?? '',
    primary: a.name || a.uid,
    caption: qoderModeLabel(a.mode),
    current: a.uid === qoder?.activeUid,
    onClick: async () => {
      setQoderPicker(false)
      if (a.uid && a.uid !== qoder?.activeUid) await qoderSwitch(a.uid)
    },
    onDelete: a.uid
      ? () => {
          if (confirm(`删除账号「${a.name || a.uid}」？本地令牌会一起清掉。`)) void qoderRemove(a.uid!)
        }
      : undefined,
  }))

  return (
    <>
      {/* 1. 积分总计 */}
      <Card tone="normal">
        <Row gap={12}>
          <Text fs="cardTitle" bold grow>
            积分总计
          </Text>
          <LinkPill onClick={() => void credits.refreshAll()} disabled={quotaLoading}>
            刷新
          </LinkPill>
        </Row>
        <Row gap={28}>
          <div>
            <Caption>Trae</Caption>
            <Text fs="metric" bold color="primary">{creditValue(snap?.trae)}{snap?.trae != null ? '' : ''}</Text>
          </div>
          <div>
            <Caption>Qoder</Caption>
            <Text fs="metric" bold color="primary">{creditValue(snap?.qoder)}</Text>
          </div>
          <div>
            <Caption>合计</Caption>
            <Text fs="metric" bold color="primary">{creditValue(snap?.total)}</Text>
          </div>
        </Row>
        <Caption style={{ opacity: 0.85 }}>{creditUpdated(snap)}{snap?.error ? `（${snap.error}）` : ''}</Caption>
      </Card>

      {/* 2. TRAE 账号 */}
      <SectionTitle>TRAE 账号</SectionTitle>
      {trae?.logged?.length ? (
        <Card tone="normal">
          <Row gap={10}>
            <Text fs="cardTitle" bold grow>
              {activeTrae?.nickname || 'TRAE 账号'}
            </Text>
            <Chip kind="ok">当前使用</Chip>
          </Row>
          <LabelValue label="区域" value={<Text selectable>{`${regionLabelShort(activeTrae?.regionId)}${activeTrae?.regionId === 'sg' ? '（未开放）' : ''}`}</Text>} />
          <LabelValue label="UID" value={<Text selectable>{activeTrae?.uid}</Text>} />
          <LabelValue label="Token" value={tokenRenewLabel(activeTrae, trae?.authFailedUid)} />
          <Caption>积分：{creditValue(snap?.trae)}</Caption>
          <Row gap={10} top={12}>
            <ActionButton variant="outlined" disabled={checkinBusy} onClick={() => void credits.claimAllTrae()}>
              {checkinBusy ? '签到中…' : activeTrae ? '签到' : '未登录'}
            </ActionButton>
          </Row>
          <AccountBar label="账户选择" onClick={() => setTraePicker(true)} />
          <Row gap={10} top={10}>
            <ActionButton onClick={() => void traeLoginStart(trae?.regionId || 'cn')}>登录账号</ActionButton>
            <ActionButton variant="outlined" onClick={() => setLoginUrlOpen(true)}>
              粘贴回调链接
            </ActionButton>
          </Row>
        </Card>
      ) : (
        <Card tone="soft">
          <Text fs="cardTitle" bold>
            未添加账号
          </Text>
          <Row gap={10}>
            <ActionButton onClick={() => void traeLoginStart(trae?.regionId || 'cn')}>登录账号</ActionButton>
            <ActionButton variant="outlined" onClick={() => setLoginUrlOpen(true)}>
              粘贴回调链接
            </ActionButton>
          </Row>
        </Card>
      )}

      {/* 3. Qoder 账号 */}
      <SectionTitle>Qoder 账号</SectionTitle>
      {qoder?.logged?.length ? (
        <Card tone="normal">
          <Row gap={10}>
            <Text fs="cardTitle" bold grow>
              {activeQoder?.name || 'Qoder 账号'}
            </Text>
            <Chip kind="ok">当前使用</Chip>
          </Row>
          <LabelValue label="区域" value={qoderModeLabel(activeQoder?.mode)} />
          <LabelValue label="UID" value={<Text selectable>{activeQoder?.uid}</Text>} />
          {activeQoder?.email ? <LabelValue label="邮箱" value={!snap ? '' : <Text>{activeQoder.email}</Text>} /> : null}
          <Caption>积分</Caption>
          <Text fs="metric" bold color="primary">
            {creditValue(snap?.qoder)}
          </Text>
          {activeQoder?.uid ? (
            <Row gap={10} top={12}>
              <ActionButton disabled={qoderClaimBusy} onClick={() => void credits.claimOneQoder(activeQoder.uid!)}>
                {qoderClaimBusy ? '领取中…' : '领取'}
              </ActionButton>
              <LinkPill
                onClick={async () => {
                  const cur = await qoderFingerprintGet(activeQoder.uid!)
                  setFp({ id: cur?.id ?? '', token: cur?.token ?? '', machineType: cur?.machineType ?? '' })
                  setFpOpen(true)
                }}
              >
                设备身份：本机派生
              </LinkPill>
            </Row>
          ) : null}
          <AccountBar label="账户选择" onClick={() => setQoderPicker(true)} />
          <Row gap={10} top={10}>
            <ActionButton
              onClick={() => {
                qoderLoginGuide()
                setPatOpen(true)
              }}
            >
              登录 Qoder
            </ActionButton>
            <ActionButton variant="outlined" onClick={() => setPatOpen(true)}>
              回填令牌
            </ActionButton>
          </Row>
        </Card>
      ) : (
        <Card tone="soft">
          <Text fs="cardTitle" bold>
            未添加账号
          </Text>
          <Row gap={10}>
            <ActionButton
              onClick={() => {
                qoderLoginGuide()
                setPatOpen(true)
              }}
            >
              登录 Qoder
            </ActionButton>
            <ActionButton variant="outlined" onClick={() => setPatOpen(true)}>
              回填令牌
            </ActionButton>
          </Row>
        </Card>
      )}

      {/* 4. 聊天转发后端 */}
      <SectionTitle>聊天转发后端</SectionTitle>
      <Row gap={8}>
        {(['trae', 'qoder', 'auto'] as const).map((b) => (
          <ActionButton key={b} style={{ flex: 1 }} variant={backend === b ? 'filled' : 'outlined'} onClick={() => void backendSet(b)}>
            {b === 'trae' ? 'TRAE' : b === 'qoder' ? 'Qoder' : '自动'}
          </ActionButton>
        ))}
      </Row>

      {/* 5. 自动换号 */}
      <SectionTitle>自动换号</SectionTitle>
      <Card tone="normal">
        <Row gap={10}>
          <Text bold grow>
            当前号用完就换
          </Text>
          <ActionButton variant={auto.enabled ? 'filled' : 'outlined'} onClick={() => void auto.toggle()}>
            {auto.enabled ? '已开启' : '已关闭'}
          </ActionButton>
        </Row>
        <Row gap={10} top={12}>
          <Text bold grow>
            换号阈值
          </Text>
          <Chip kind="info">{`≤ ${auto.threshold}`}</Chip>
        </Row>
        <Row gap={8} top={10}>
          {[5, 10, 15, 20].map((n) => (
            <ActionButton key={n} style={{ flex: 1 }} variant={auto.threshold === n ? 'filled' : 'outlined'} onClick={() => void auto.setThreshold(n)}>
              {n}
            </ActionButton>
          ))}
        </Row>
        <Caption>只在同一个平台内换号；换完立刻刷新新号的余额。不做定时检测。</Caption>
      </Card>

      {/* 6. 账户导出 */}
      <SectionTitle>账户导出</SectionTitle>
      <Row gap={8}>
        <ActionButton variant="outlined" style={{ flex: 1 }} onClick={() => void backupExport()}>
          导出
        </ActionButton>
        <ActionButton
          variant="outlined"
          style={{ flex: 1 }}
          onClick={async () => {
            const text = prompt('粘贴账号 JSON')
            if (text) await backupImport(text)
          }}
        >
          导入
        </ActionButton>
      </Row>

      {/* 7. 一键签到 */}
      <SectionTitle>一键签到</SectionTitle>
      <Card tone="normal">
        <ActionButton
          style={{ width: "100%" }}
          disabled={checkinBusy || qoderClaimBusy}
          onClick={async () => {
            if (!(trae?.logged?.length ?? 0) && !(qoder?.logged?.length ?? 0)) {
              toast('没有可签到的账号')
              return
            }
            await credits.claimAllTrae()
            await credits.claimAllQoder()
          }}
        >
          一键签到全部账号
        </ActionButton>
        <Row gap={6} top={8} wrap>
          <LinkPill
            onClick={async () => {
              const t = await credits.diagnoseTrae()
              if (t) setDiag({ title: 'Trae 账号诊断', text: t })
            }}
          >
            诊断 Trae 账号
          </LinkPill>
          <LinkPill
            onClick={async () => {
              const t = await credits.diagnoseQoder()
              if (t) setDiag({ title: '领取链路诊断', text: t })
            }}
          >
            诊断领取链路
          </LinkPill>
        </Row>
        <Caption>
          {`Trae：${traeSchedule.enabled ? `定时 ${traeSchedule.hhmm}` : '定时未开启'}　·　Qoder：${qoderSchedule.enabled ? `定时 ${qoderSchedule.hhmm}` : '定时未开启'}`}
        </Caption>
        <Row gap={6} top={10} wrap>
          <LinkPill onClick={() => setTimeOpen('trae')}>Trae 定时</LinkPill>
          <LinkPill onClick={() => setTimeOpen('qoder')}>Qoder 定时</LinkPill>
        </Row>
      </Card>

      {traePicker ? <PickerDialog title="选择 Trae 账号" rows={traeRows} onClose={() => setTraePicker(false)} /> : null}
      {qoderPicker ? <PickerDialog title="选择 Qoder 账号" rows={qoderRows} onClose={() => setQoderPicker(false)} /> : null}

      {loginUrlOpen && (
        <ConfirmDialog
          title="粘贴回调链接"
          message="用系统浏览器打开登录链接登录后，把地址栏里那串 http://127.0.0.1:18080/authorize?... 完整粘进来。"
          okText="登录"
          content={
            <textarea
              style={{ width: '100%', minHeight: 72 }}
              placeholder="粘贴登录后跳转的完整链接"
              value={cbUrl}
              onChange={(e) => setCbUrl(e.target.value)}
            />
          }
          onOk={async () => {
            if (await traeLoginSubmit(cbUrl)) setLoginUrlOpen(false)
          }}
          onCancel={() => setLoginUrlOpen(false)}
        />
      )}

      {patOpen && (
        <ConfirmDialog
          title="粘贴 Qoder PAT 登录"
          message="在 Qoder 账号集成页创建个人访问令牌后粘贴。"
          okText="登录"
          content={
            <>
              <textarea style={{ width: '100%', minHeight: 56 }} placeholder="粘贴 Qoder 个人访问令牌 pt-…" value={pat} onChange={(e) => setPat(e.target.value)} />
              <Caption>令牌属于哪个区域？</Caption>
              <Row gap={6}>
                <ActionButton style={{ flex: 1 }} variant={patMode === 'global' ? 'filled' : 'outlined'} onClick={() => setPatMode('global')}>
                  国际版
                </ActionButton>
                <ActionButton style={{ flex: 1 }} variant={patMode === 'cn' ? 'filled' : 'outlined'} onClick={() => setPatMode('cn')}>
                  国内版
                </ActionButton>
              </Row>
            </>
          }
          onOk={async () => {
            const mode = patMode === 'global' ? 'global' : 'cn'
            if (await qoderLoginPat(pat, mode)) {
              setPat('')
              setPatOpen(false)
            }
          }}
          onCancel={() => setPatOpen(false)}
        />
      )}

      {fpOpen && (
        <ConfirmDialog
          title="Qoder 设备身份"
          okText="保存"
          content={
            <>
              <input style={{ width: '100%', marginBottom: 6 }} placeholder="cosy-machineid" value={fp.id} onChange={(e) => setFp({ ...fp, id: e.target.value })} />
              <input style={{ width: '100%', marginBottom: 6 }} placeholder="cosy-machinetoken" value={fp.token} onChange={(e) => setFp({ ...fp, token: e.target.value })} />
              <input style={{ width: '100%' }} placeholder="cosy-machinetype（默认空）" value={fp.machineType} onChange={(e) => setFp({ ...fp, machineType: e.target.value })} />
            </>
          }
          onOk={async () => {
            const r = await qoderFingerprintSet(activeQoder?.uid ?? '', fp)
            if (r === true) {
              toast('已保存抓包指纹')
              setFpOpen(false)
            } else {
              toast(r)
            }
          }}
          onCancel={() => setFpOpen(false)}
        />
      )}

      {timeOpen && (
        <TimePickerDialog
          title="定时领取时间"
          value={timeOpen === 'trae' ? traeSchedule.hhmm : qoderSchedule.hhmm}
          onOk={async (hhmm) => {
            const platform = timeOpen
            setTimeOpen(null)
            await scheduleSet(platform, { hhmm })
            const r = await scheduleGet(platform)
            if (r) (platform === 'trae' ? setTraeSchedule : setQoderSchedule)(r)
          }}
          onCancel={() => setTimeOpen(null)}
        />
      )}

      {diag && <DiagnoseDialog title={diag.title} text={diag.text} onClose={() => setDiag(null)} />}
    </>
  )
}

/// 账户选择行里的每号积分读法：后端快照当前只有平台合计，先显示合计占位（后续按 uid 细化）
function snapItemsValue(snap: ReturnType<typeof useCredits.getState>['snapshot'], platform: string, uid?: string): string {
  void platform
  void uid
  return snap ? '—' : '—'
}
