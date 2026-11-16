import * as React from "react"

import { Empty, Failure, Page, PageHeader } from "@/components/layout/page"
import { Skeleton } from "@/components/ui/skeleton"
import { useAdmin } from "@/hooks/use-admin"
import { admin } from "@/lib/admin"
import { formatExact, formatRelative } from "@/lib/format"

/**
 * The webhook endpoints registered per key.
 *
 * Endpoints are key-scoped like everything else, and nothing hands this page a key, so
 * it reads the key list first and then every key's endpoints. That is a request per key,
 * which is fine for a console on localhost looking at a handful of test keys.
 */
export function WebhooksPage() {
  const keys = useAdmin("keys", () => admin.keys(), { pollMs: 5000 })

  return (
    <Page>
      <PageHeader
        description="Where delivery events are posted, signed the way Resend signs them. The secret is shown in full: nothing here is secret, and a test that cannot read it cannot check a signature."
        title="Webhooks"
      />

      {keys.isLoading ? (
        <Skeleton className="h-40 w-full" />
      ) : keys.error ? (
        <Failure detail={keys.error.message} title="Could not read the keys" />
      ) : keys.data && keys.data.count > 0 ? (
        <div className="space-y-5">
          {keys.data.data.map((key) => (
            <KeyEndpoints apiKey={key.api_key} key={key.api_key} />
          ))}
        </div>
      ) : (
        <Empty title="No key has been used yet">
          Register an endpoint with <span className="font-mono">POST /_admin/webhooks</span>, naming
          the key it belongs to.
        </Empty>
      )}
    </Page>
  )
}

function KeyEndpoints({ apiKey }: { apiKey: string }) {
  const endpoints = useAdmin(`webhooks:${apiKey}`, () => admin.webhooks(apiKey), { pollMs: 5000 })

  // A key with no endpoints is the common case, and a row saying so for every key would
  // bury the ones that have them.
  if (!endpoints.data || endpoints.data.count === 0) {
    return null
  }

  return (
    <div className="overflow-hidden rounded-lg border border-border/60">
      <div className="border-border/60 border-b px-3 py-2 font-mono text-[12px] text-muted-foreground">
        {apiKey}
      </div>
      <div className="divide-y divide-border/60">
        {endpoints.data.data.map((endpoint) => (
          <div className="px-3 py-3" key={endpoint.id}>
            <div className="flex items-baseline justify-between gap-4">
              <span className="min-w-0 truncate font-mono text-[12.5px]">{endpoint.url}</span>
              <span
                className="shrink-0 text-[11.5px] text-muted-foreground tabular-nums"
                title={formatExact(endpoint.created_at)}
              >
                {formatRelative(endpoint.created_at)}
              </span>
            </div>

            <div className="mt-2 flex flex-wrap items-center gap-1.5">
              {endpoint.events.length === 0 ? (
                <span className="rounded bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">
                  every event
                </span>
              ) : (
                endpoint.events.map((event) => (
                  <span
                    className="rounded bg-muted px-1.5 py-0.5 font-mono text-[11px]"
                    key={event}
                  >
                    {event}
                  </span>
                ))
              )}
            </div>

            <Secret value={endpoint.signing_secret} />
          </div>
        ))}
      </div>
    </div>
  )
}

/** The signing secret, hidden until asked for — not because it is secret, but because a
 * wall of base64 makes the list unreadable. */
function Secret({ value }: { value: string }) {
  const [shown, setShown] = React.useState(false)

  return (
    <button
      className="mt-2 block w-full truncate rounded-md border border-border/50 bg-muted/20 px-2 py-1 text-left font-mono text-[11px] text-muted-foreground transition-colors hover:border-border hover:bg-muted/40"
      onClick={() => setShown((current) => !current)}
      type="button"
    >
      {shown ? value : "whsec_··· — click to reveal the signing secret"}
    </button>
  )
}
