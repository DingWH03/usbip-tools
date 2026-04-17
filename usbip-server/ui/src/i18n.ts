export type Locale = 'zh-CN' | 'en'

const STORAGE_KEY = 'usbip-server.locale'

type Dict = Record<string, string>

const zhCN: Dict = {
  appSubhead: 'USB/IP 设备管理 · bind & rules',
  refresh: '刷新',
  applyRules: '应用规则',
  onlyFree: '只看可用',
  devices: '设备',
  rules: '规则',
  busid: 'busid',
  device: '设备',
  identifiers: '标识',
  status: '状态',
  notExported: '未导出',
  inUse: '占用中',
  free: '可用',
  bind: '绑定',
  unbind: '解绑',
  noDevices: '没有设备',
  matchNote: '匹配 / 备注',
  disable: '禁用',
  enable: '启用',
  del: '删除',
  noRules: '没有规则',
  rawJson: '原始 JSON',
  deleteRuleConfirm: '确定删除这条规则？',
  filterPlaceholder: '筛选 busid/serial/vid:pid…',
  language: '语言',
}

const en: Dict = {
  appSubhead: 'USB/IP device manager · bind & rules',
  refresh: 'refresh',
  applyRules: 'apply rules',
  onlyFree: 'only free',
  devices: 'devices',
  rules: 'rules',
  busid: 'busid',
  device: 'device',
  identifiers: 'identifiers',
  status: 'status',
  notExported: 'not exported',
  inUse: 'in use',
  free: 'free',
  bind: 'bind',
  unbind: 'unbind',
  noDevices: 'no devices',
  matchNote: 'match / note',
  disable: 'disable',
  enable: 'enable',
  del: 'del',
  noRules: 'no rules',
  rawJson: 'raw JSON',
  deleteRuleConfirm: 'delete rule?',
  filterPlaceholder: 'filter busid/serial/vid:pid…',
  language: 'lang',
}

function dictFor(locale: Locale): Dict {
  return locale === 'zh-CN' ? zhCN : en
}

export function loadLocale(): Locale {
  const v = localStorage.getItem(STORAGE_KEY)
  if (v === 'zh-CN' || v === 'en') return v
  const nav = navigator.language
  return nav.toLowerCase().startsWith('zh') ? 'zh-CN' : 'en'
}

export function saveLocale(locale: Locale) {
  localStorage.setItem(STORAGE_KEY, locale)
}

export function t(locale: Locale, key: keyof typeof zhCN): string {
  const d = dictFor(locale)
  return d[key] ?? zhCN[key] ?? String(key)
}

