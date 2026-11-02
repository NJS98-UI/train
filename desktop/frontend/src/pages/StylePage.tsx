// 二级页「外观风格」——按 ui-keepalive.md §2.6 复刻：4 行风格选择（色样 + 名称/说明 + 使用中徽章）
// + 亮暗模式卡（3 行单选）+ 尾注。选择立即生效并写回 config（{style:{styleId,mode}}）。
import { Card, SectionTitle, Chip, Text, Caption } from '../components'
import { MODES, STYLES, cssVar, findStyle, resolveIsNight, type ModeId } from '../theme/styles'
import { useUiStyle } from '../store/uiStyle'
import { toast } from '../store/toast'

function Swatch({ styleId, isNight }: { styleId: string; isNight: boolean }) {
  // 色样底 = 该风格自己的 primary，内胶囊 = 对应 on_primary（§2.6 styleSwatch）
  const def = findStyle(styleId)
  const palette = isNight ? def.dark : def.light
  return (
    <span className="tw-swatch" style={{ background: palette.primary, borderColor: palette.outlineVariant }}>
      <span className="tw-swatch__pill" style={{ background: palette.onPrimary }} />
    </span>
  )
}

export function StylePage() {
  const styleId = useUiStyle((s) => s.styleId)
  const mode = useUiStyle((s) => s.mode)
  const prefersDark = useUiStyle((s) => s.prefersDark)
  const setStyle = useUiStyle((s) => s.setStyle)
  const setMode = useUiStyle((s) => s.setMode)

  const isNight = resolveIsNight(mode, prefersDark)

  return (
    <>
      <SectionTitle>风格</SectionTitle>
      {STYLES.map((s) => {
        const selected = s.id === styleId
        return (
          <button
            key={s.id}
            type="button"
            className={`tw-styleopt${selected ? ' tw-styleopt--selected tw-styleopt--raised' : ''}`}
            onClick={() => {
              if (selected) return
              void setStyle(s.id)
              toast(`已切换为「${s.label}」`)
            }}
          >
            <Swatch styleId={s.id} isNight={isNight} />
            <span style={{ flex: '1 1 auto', display: 'flex', flexDirection: 'column', gap: 2 }}>
              <Text fs="cardTitle" bold color={selected ? 'primary' : 'onSurface'}>
                {s.label}
              </Text>
              <Caption>{s.note}</Caption>
            </span>
            {selected && <Chip kind="info">{'✓ 使用中'}</Chip>}
          </button>
        )
      })}

      <SectionTitle>亮暗模式</SectionTitle>
      <Card tone="normal">
        {MODES.map((m, i) => {
          const selected = m.id === mode
          return (
            <div key={m.id} className="tw-stack">
              {i > 0 && <div className="tw-divider" />}
              <button
                type="button"
                className="tw-modeopt"
                onClick={() => {
                  void setMode(m.id as ModeId)
                  toast(`亮暗模式：${m.label}`)
                }}
              >
                <span style={{ flex: '1 1 auto', display: 'flex', flexDirection: 'column', gap: 2 }}>
                  <Text fs="body" bold color={selected ? 'primary' : 'onSurface'}>
                    {m.label}
                  </Text>
                  <Caption>{m.note}</Caption>
                </span>
                {selected ? (
                  <span className="tw-radio" style={{ background: cssVar('primary'), borderColor: cssVar('primary') }}>
                    <span className="tw-radio__inner" />
                  </span>
                ) : (
                  <span className="tw-radio" />
                )}
              </button>
            </div>
          )
        })}
      </Card>

      <Caption>切换后立即生效，并会记住你的选择；重开 App 仍是这套。</Caption>
      <Caption>当前解析结果：{isNight ? '深色' : '浅色'}（{findStyle(styleId).label}）</Caption>
    </>
  )
}
