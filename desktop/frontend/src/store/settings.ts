// AppConfig（config_get / config_update）：字段与 tw-core/src/config.rs 的 camelCase serde 一一对应。
// config_update 的入参是整份 config（后端 *c = patch），因此前端始终 merge 后回传。
import { create } from 'zustand'
import { call } from '../ipc'

export interface StyleField {
  styleId: string
  mode: string
}

export interface AppConfig {
  gatewayBackend: string
  traeRegion: string
  autoSwitchEnabled: boolean
  autoSwitchThreshold: number
  checkin: {
    traeEnabled: boolean
    traeTime: string
    qoderEnabled: boolean
    qoderTime: string
  }
  style: StyleField
  closeToTray: boolean
  autostart: boolean
  appInstallId: string
}

export const DEFAULT_CONFIG: AppConfig = {
  gatewayBackend: 'auto',
  traeRegion: 'cn',
  autoSwitchEnabled: true,
  autoSwitchThreshold: 5,
  checkin: { traeEnabled: false, traeTime: '08:00', qoderEnabled: false, qoderTime: '09:00' },
  style: { styleId: 'indigo', mode: 'system' },
  closeToTray: true,
  autostart: false,
  appInstallId: '',
}

interface SettingsState {
  config: AppConfig
  loaded: boolean
  error: string | null
  load: () => Promise<AppConfig | null>
  /// 局部改写：merge 到当前 config 后整体 config_update，失败返回 null（调用方决定占位）
  patch: (partial: Partial<AppConfig>) => Promise<AppConfig | null>
}

export const useSettings = create<SettingsState>((set, get) => ({
  config: DEFAULT_CONFIG,
  loaded: false,
  error: null,
  async load() {
    try {
      const config = await call<AppConfig>('config_get')
      set({ config, loaded: true, error: null })
      return config
    } catch (e) {
      set({ loaded: true, error: (e as Error).message })
      return null
    }
  },
  async patch(partial) {
    const merged: AppConfig = { ...get().config, ...partial }
    try {
      const config = await call<AppConfig>('config_update', { config: merged })
      set({ config, loaded: true, error: null })
      return config
    } catch (e) {
      set({ error: (e as Error).message })
      return null
    }
  },
}))

/// 外观读写走 config_get/config_update 的 style 字段（{style:{styleId,mode}}）
export async function updateStyleField(styleId: string, mode: string): Promise<void> {
  await useSettings.getState().patch({ style: { styleId, mode } })
}
