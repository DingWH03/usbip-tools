import type { DeviceView, RulesFile } from './types'

async function fetchJSON<T>(url: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(url, init)
  if (!res.ok) {
    const msg = await res.text().catch(() => '')
    throw new Error(msg || `${res.status} ${res.statusText}`)
  }
  const ct = res.headers.get('content-type') ?? ''
  if (!ct.includes('application/json')) {
    return null as T
  }
  const text = await res.text()
  if (!text.trim()) {
    return null as T
  }
  return JSON.parse(text) as T
}

export function apiDevices(signal?: AbortSignal) {
  return fetchJSON<DeviceView[]>('/api/devices', { signal })
}

export function apiRules(signal?: AbortSignal) {
  return fetchJSON<RulesFile>('/api/rules', { signal })
}

export async function apiBind(busid: string) {
  await fetchJSON<unknown>('/api/bind', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ busid }),
  })
}

export async function apiUnbind(busid: string) {
  await fetchJSON<unknown>('/api/unbind', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ busid }),
  })
}

export async function apiApply() {
  await fetchJSON<unknown>('/api/apply', { method: 'POST' })
}

export async function apiToggleRule(id: string, enabled: boolean) {
  await fetchJSON<unknown>('/api/rules/toggle', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id, enabled }),
  })
}

export async function apiDeleteRule(id: string) {
  await fetchJSON<unknown>('/api/rules/delete', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id }),
  })
}

