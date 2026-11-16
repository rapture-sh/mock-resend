/** Formatting for the things this console shows: addresses, sizes, timestamps. */

const UNITS = ["B", "KB", "MB", "GB"] as const

/** A byte count at a glance, for an attachment. */
export function formatBytes(bytes: number): string {
  if (bytes === 0) {
    return "0 B"
  }

  const exponent = Math.min(Math.floor(Math.log10(bytes) / 3), UNITS.length - 1)
  const value = bytes / 1000 ** exponent
  const digits = exponent === 0 || value >= 100 ? 0 : 1

  return `${value.toFixed(digits)} ${UNITS[exponent]}`
}

/**
 * How long ago, in the shortest form that is still unambiguous.
 *
 * A console open beside a test run is mostly looking at the last few seconds, which is
 * why this goes finer than it goes coarse.
 */
export function formatRelative(iso: string, now = Date.now()): string {
  const then = new Date(iso).getTime()

  if (Number.isNaN(then)) {
    return "unknown"
  }

  const seconds = Math.max(0, Math.round((now - then) / 1000))

  if (seconds < 5) {
    return "just now"
  }
  if (seconds < 60) {
    return `${seconds}s ago`
  }
  if (seconds < 3600) {
    return `${Math.floor(seconds / 60)}m ago`
  }
  if (seconds < 86400) {
    return `${Math.floor(seconds / 3600)}h ago`
  }

  return `${Math.floor(seconds / 86400)}d ago`
}

/** A full timestamp, for a tooltip behind the relative one. */
export function formatExact(iso: string): string {
  const date = new Date(iso)

  if (Number.isNaN(date.getTime())) {
    return iso
  }

  return date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" })
}

/** Just the clock time, for a dense list where the date is usually today. */
export function formatTime(iso: string): string {
  const date = new Date(iso)

  if (Number.isNaN(date.getTime())) {
    return iso
  }

  return date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
}

/**
 * The display name out of `Name <address>`, or the address when there is none.
 *
 * A list is easier to scan by name, and the full address is a hover away.
 */
export function addressName(address: string): string {
  const match = address.match(/^\s*"?([^"<]*?)"?\s*<([^>]+)>\s*$/)

  if (match?.[1] && match[1].trim() !== "") {
    return match[1].trim()
  }

  return bareAddress(address)
}

/** The address out of `Name <address>`, or the whole string. */
export function bareAddress(address: string): string {
  const match = address.match(/<([^>]+)>/)

  return (match?.[1] ?? address).trim()
}

/** Every recipient on one line, with the overflow counted rather than listed. */
export function summariseRecipients(to: string[]): string {
  if (to.length === 0) {
    return "no recipients"
  }

  const first = addressName(to[0] ?? "")

  return to.length === 1 ? first : `${first} +${to.length - 1}`
}

/**
 * Whether a delivery event means something went wrong.
 *
 * Used for colour only: the event name itself is always shown, because that is the
 * thing a caller's code branches on.
 */
export function isFailureEvent(event: string): boolean {
  return event === "bounced" || event === "failed" || event === "complained"
}

/** Whether a delivery event means the message is still on its way. */
export function isPendingEvent(event: string): boolean {
  return event === "scheduled" || event === "queued" || event === "delivery_delayed"
}
