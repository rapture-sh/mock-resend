import { useNavigate, useSearch } from "@tanstack/react-router"
import { X } from "lucide-react"

import { Empty, Failure, Page, PageHeader } from "@/components/layout/page"
import { MailTable } from "@/components/mail/mail-table"
import { Skeleton } from "@/components/ui/skeleton"
import { useAdmin } from "@/hooks/use-admin"
import { admin } from "@/lib/admin"

/**
 * Everything captured, newest first.
 *
 * Polls, because the usual way to look at this is with a test run going in another
 * window. `useAdmin` only shows a loading state on the first read, so a poll does not
 * flash a skeleton over a list someone is reading.
 */
export function MailPage() {
  const { api_key: apiKey } = useSearch({ from: "/mail" })
  const navigate = useNavigate()

  const mail = useAdmin(`mailbox:${apiKey ?? "all"}`, () => admin.mailbox(apiKey), {
    pollMs: 2000,
  })

  return (
    <Page>
      <PageHeader
        description={
          apiKey
            ? "Captured under one key. Nothing here was delivered."
            : "Everything captured, across every API key. Nothing here was delivered."
        }
        title="Mail"
      />

      {apiKey ? (
        <button
          className="mb-4 inline-flex items-center gap-1.5 rounded-md border border-border/60 px-2.5 py-1 text-[12px] transition-colors hover:bg-accent"
          onClick={() => navigate({ to: "/mail", search: {} })}
          type="button"
        >
          <span className="font-mono">{apiKey}</span>
          <X aria-hidden="true" className="size-3.5" strokeWidth={2} />
        </button>
      ) : null}

      {mail.isLoading ? (
        <Skeleton className="h-64 w-full" />
      ) : mail.error ? (
        <Failure detail={mail.error.message} title="Could not read the mailbox" />
      ) : mail.data && mail.data.count > 0 ? (
        <div className="overflow-hidden rounded-lg border border-border/60">
          <MailTable emails={mail.data.data} showKey={apiKey === undefined} />
        </div>
      ) : (
        <Empty title="Nothing captured yet">
          Point a Resend client at <span className="font-mono">http://localhost:9100</span> with any{" "}
          <span className="font-mono">re_</span>-prefixed key, or send over SMTP on port 1025. Mail
          shows up here the moment it arrives.
        </Empty>
      )}
    </Page>
  )
}
