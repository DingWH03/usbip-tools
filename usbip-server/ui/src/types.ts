export type DeviceView = {
  busid: string
  serial: string | null
  devpath: string | null
  vid: number | null
  pid: number | null
  manufacturer: string | null
  product: string | null
  exported: boolean
  in_use: boolean
  remote: string | null
}

export type MatchSpec = {
  serial: string | null
  devpath: string | null
  vid: number | null
  pid: number | null
}

export type Rule = {
  id: string
  enabled: boolean
  note: string | null
  match: MatchSpec
  action: 'bind'
}

export type RulesFile = {
  rules: Rule[]
}

