import { Link } from "@tanstack/react-router"

import { Empty, Failure, Page, PageHeader, Stat } from "@/components/layout/page"
import { MailTable } from "@/components/mail/mail-table"
import { Skeleton } from "@/components/ui/skeleton"
import { useAdmin } from "@/hooks/use-admin"
import { admin } from "@/lib/admin"

/** What is here, and the last few messages. */
export function OverviewPage() {
  const keys = useAdmin("keys", () => admin.keys(), { pollMs: 3000 })
  const mail = useAdmin("recent", () => admin.mailbox(undefined, 10), { pollMs: 2000 })

  const captured = keys.data?.data.reduce((total, key) => total + key.captured, 0) ?? 0

  return (
    <Page>
      <PageHeader
        description="A Resend-compatible API that captures every message instead of delivering it."
        title="Overview"
      />

      {keys.error ? (
        <Failure detail={keys.error.message} title="mock-resend is not answering" />
      ) : (
        <>
          <div className="mb-9 grid grid-cols-2 gap-8 sm:grid-cols-4">
            <Stat
              label="Captured"
              value={keys.isLoading ? <Skeleton className="h-5 w-10" /> : captured}
            />
            <Stat
              label="API keys"
              value={keys.isLoading ? <Skeleton className="h-5 w-10" /> : (keys.data?.count ?? 0)}
            />
            <Stat label="API" value={<span className="font-mono text-[13px]">:9100</span>} />
            <Stat label="SMTP" value={<span className="font-mono text-[13px]">:1025</span>} />
          </div>

          <div className="mb-3 flex items-baseline justify-between">
            <h2 className="font-medium text-[15px]">Recent</h2>
            <Link
              className="text-[12.5px] text-muted-foreground transition-colors hover:text-foreground"
              to="/mail"
            >
              All mail
            </Link>
          </div>

          {mail.isLoading ? (
            <Skeleton className="h-48 w-full" />
          ) : mail.data && mail.data.count > 0 ? (
            <div className="overflow-hidden rounded-lg border border-border/60">
              <MailTable emails={mail.data.data} />
            </div>
          ) : (
            <Empty title="Nothing captured yet">
              Point a Resend client at <span className="font-mono">http://localhost:9100</span> with
              any <span className="font-mono">re_</span>-prefixed key. The key is the namespace, so
              give each test its own.
            </Empty>
          )}
        </>
      )}
    </Page>
  )
}
