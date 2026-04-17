import { useEffect, useMemo, useRef, useState } from 'react'
import type { DeviceView, RulesFile } from './types'
import { apiBind, apiDeleteRule, apiDevices, apiRules, apiToggleRule, apiUnbind, apiApply } from './api'
import { loadLocale, saveLocale, t, type Locale } from './i18n'

function fmtVidPid(vid?: number | null, pid?: number | null) {
  if (vid == null || pid == null) return ''
  return vid.toString(16).padStart(4, '0') + ':' + pid.toString(16).padStart(4, '0')
}

function deviceSearchText(d: DeviceView) {
  const vidpid = d.vid != null && d.pid != null ? fmtVidPid(d.vid, d.pid) : ''
  return [d.busid, d.serial, d.manufacturer, d.product, vidpid].filter(Boolean).join(' ').toLowerCase()
}

function matchText(m: RulesFile['rules'][number]['match']) {
  const parts: string[] = []
  if (m.serial) parts.push(`serial=${m.serial}`)
  if (m.devpath) parts.push(`devpath=${m.devpath.split('/').pop()}`)
  if (m.vid != null && m.pid != null) parts.push(`vidpid=${fmtVidPid(m.vid, m.pid)}`)
  return parts.join(' · ') || '(empty)'
}

export default function App() {
  const [devices, setDevices] = useState<DeviceView[]>([])
  const [rules, setRules] = useState<RulesFile>({ rules: [] })
  const [q, setQ] = useState('')
  const [onlyFree, setOnlyFree] = useState(false)
  const [loading, setLoading] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  const [locale, setLocale] = useState<Locale>(() => loadLocale())

  const abortRef = useRef<AbortController | null>(null)

  async function loadData() {
    abortRef.current?.abort()
    const ac = new AbortController()
    abortRef.current = ac
    setLoading(true)
    setErr(null)
    try {
      const [d, r] = await Promise.all([apiDevices(ac.signal), apiRules(ac.signal)])
      setDevices(d)
      setRules(r)
    } catch (e) {
      if ((e as Error).name !== 'AbortError') setErr((e as Error).message ?? String(e))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    loadData()
    // SSE: 任意状态变化后服务端会广播一条 update，我们只需要刷新数据即可。
    const es = new EventSource('/api/events')
    es.addEventListener('update', () => void loadData())
    es.onerror = () => {
      // 浏览器会自动重连；不需要在这里做额外处理。
    }
    return () => {
      es.close()
      abortRef.current?.abort()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => {
    saveLocale(locale)
    document.documentElement.lang = locale
    document.title = 'usbip-server'
  }, [locale])

  const filteredDevices = useMemo(() => {
    const qq = q.trim().toLowerCase()
    return devices.filter((d) => {
      if (qq && !deviceSearchText(d).includes(qq)) return false
      if (onlyFree && (d.in_use || !d.exported)) return false
      return true
    })
  }, [devices, q, onlyFree])

  async function run(action: () => Promise<void>) {
    setErr(null)
    try {
      await action()
      await loadData()
    } catch (e) {
      setErr((e as Error).message ?? String(e))
      alert((e as Error).message ?? String(e))
    }
  }

  return (
    <div className="app">
      <header className="topBar">
        <div className="logoArea">
          <h1>usbip-server</h1>
          <div className="subhead">{t(locale, 'appSubhead')}</div>
        </div>

        <div className="controls">
          <div className="searchWrapper">
            <input
              type="search"
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder={t(locale, 'filterPlaceholder')}
              aria-label="filter"
            />
            <button onClick={() => void loadData()} title={t(locale, 'refresh')} disabled={loading}>
              ↻
            </button>
          </div>
          <button className="primary" onClick={() => void run(() => apiApply())} disabled={loading}>
            {t(locale, 'applyRules')}
          </button>
          <label className="onlyFree" title={t(locale, 'onlyFree')}>
            <input type="checkbox" checked={onlyFree} onChange={(e) => setOnlyFree(e.target.checked)} />{' '}
            {t(locale, 'onlyFree')}
          </label>
          <label className="langPicker" title={t(locale, 'language')}>
            <span className="muted">{t(locale, 'language')}</span>
            <select
              value={locale}
              onChange={(e) => setLocale(e.target.value as Locale)}
              aria-label="language"
            >
              <option value="zh-CN">中文</option>
              <option value="en">EN</option>
            </select>
          </label>
        </div>
      </header>

      {err ? <div className="errorBanner">{err}</div> : null}

      <main className="dashboard">
        <section className="card">
          <div className="cardHeader">
            <h2>{t(locale, 'devices')}</h2>
            <span className="muted">
              {filteredDevices.length}/{devices.length}
            </span>
          </div>
          <div className="tableContainer">
            <table>
              <thead>
                <tr>
                  <th>{t(locale, 'busid')}</th>
                  <th>{t(locale, 'device')}</th>
                  <th>{t(locale, 'identifiers')}</th>
                  <th>{t(locale, 'status')}</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {filteredDevices.length === 0 ? (
                  <tr>
                    <td colSpan={5} className="empty">
                      {t(locale, 'noDevices')}
                    </td>
                  </tr>
                ) : (
                  filteredDevices.map((d) => {
                    const name = [d.manufacturer, d.product].filter(Boolean).join(' · ') || '—'
                    const vidpid = d.vid != null && d.pid != null ? fmtVidPid(d.vid, d.pid) : null
                    const idents: string[] = []
                    if (d.serial) idents.push(`serial=${d.serial}`)
                    if (d.devpath) idents.push(`devpath=${d.devpath.split('/').pop()}`)
                    if (vidpid) idents.push(`vidpid=${vidpid}`)

                    let status: { kind: 'ok' | 'warn' | 'bad'; text: string; extra?: string } = {
                      kind: 'ok',
                      text: t(locale, 'free'),
                    }
                    if (!d.exported) status = { kind: 'bad', text: t(locale, 'notExported') }
                    else if (d.in_use) status = { kind: 'warn', text: t(locale, 'inUse'), extra: d.remote ?? undefined }

                    return (
                      <tr key={d.busid}>
                        <td className="mono">{d.busid}</td>
                        <td>
                          <div className="strong">{name}</div>
                          <div className="chipGroup">
                            {vidpid ? <span className="chip">{vidpid}</span> : null}
                            {d.serial ? <span className="chip">sn:{d.serial.slice(0, 6)}…</span> : null}
                          </div>
                        </td>
                        <td className="mono small">
                          {idents.length ? idents.map((s) => <div key={s}>{s}</div>) : '—'}
                        </td>
                        <td>
                          <span className={`badge badge-${status.kind}`}>{status.text}</span>
                          {status.extra ? <span className="mono small"> {status.extra}</span> : null}
                          <div className="debugInfo mono">exported:{String(d.exported)} in_use:{String(d.in_use)}</div>
                        </td>
                        <td>
                          <div className="actionGroup">
                            {d.exported ? (
                              <button className="actionBtn" onClick={() => void run(() => apiUnbind(d.busid))}>
                                {t(locale, 'unbind')}
                              </button>
                            ) : (
                              <button className="actionBtn" onClick={() => void run(() => apiBind(d.busid))}>
                                {t(locale, 'bind')}
                              </button>
                            )}
                          </div>
                        </td>
                      </tr>
                    )
                  })
                )}
              </tbody>
            </table>
          </div>
        </section>

        <section className="card">
          <div className="cardHeader">
            <h2>{t(locale, 'rules')}</h2>
            <span className="muted">{rules.rules.length}</span>
          </div>
          <div className="tableContainer scrollY">
            <table>
              <thead>
                <tr>
                  <th>{t(locale, 'matchNote')}</th>
                  <th style={{ width: 120 }} />
                </tr>
              </thead>
              <tbody>
                {rules.rules.length === 0 ? (
                  <tr>
                    <td colSpan={2} className="empty">
                      {t(locale, 'noRules')}
                    </td>
                  </tr>
                ) : (
                  rules.rules.map((r) => {
                    const enabled = r.enabled !== false
                    return (
                      <tr key={r.id}>
                        <td>
                          <div className="ruleNote">
                            {r.note || '—'} {!enabled ? <span className="disabledTag">[disabled]</span> : null}
                          </div>
                          <div className="mono small muted">{matchText(r.match)}</div>
                          <div className="mono tiny muted">{r.id.slice(0, 8)}</div>
                        </td>
                        <td>
                          <div className="actionGroup">
                            <button
                              className="actionBtn"
                              onClick={() => void run(() => apiToggleRule(r.id, !enabled))}
                            >
                              {enabled ? t(locale, 'disable') : t(locale, 'enable')}
                            </button>
                            <button
                              className="actionBtn"
                              onClick={() =>
                                void run(async () => {
                                  if (!confirm(t(locale, 'deleteRuleConfirm'))) return
                                  await apiDeleteRule(r.id)
                                })
                              }
                            >
                              {t(locale, 'del')}
                            </button>
                          </div>
                        </td>
                      </tr>
                    )
                  })
                )}
              </tbody>
            </table>
          </div>
          <details>
            <summary>{t(locale, 'rawJson')}</summary>
            <pre>{JSON.stringify(rules, null, 2)}</pre>
          </details>
        </section>
      </main>
    </div>
  )
}
