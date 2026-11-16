import { Link } from "@tanstack/react-router"

import { Empty, Failure, Page, PageHeader } from "@/components/layout/page"
import { Skeleton } from "@/components/ui/skeleton"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { useAdmin } from "@/hooks/use-admin"
import { admin } from "@/lib/admin"
import { formatExact, formatRelative } from "@/lib/format"

/**
 * The keys that have captured mail.
 *
 * There is no key management here and there will not be: any `re_`-prefixed string is a
 * valid key and creates its namespace the first time it is used. This page is a list of
 * what has actually been used, which is the only thing the server knows.
 */
export function KeysPage() {
  const keys = useAdmin("keys", () => admin.keys(), { pollMs: 3000 })

  return (
    <Page>
      <PageHeader
        description="Any re_-prefixed string is a key, and it is the namespace everything lives in. These are the ones that have captured something."
        title="API keys"
      />

      {keys.isLoading ? (
        <Skeleton className="h-48 w-full" />
      ) : keys.error ? (
        <Failure detail={keys.error.message} title="Could not read the keys" />
      ) : keys.data && keys.data.count > 0 ? (
        <div className="overflow-hidden rounded-lg border border-border/60">
          <Table>
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                <TableHead>Key</TableHead>
                <TableHead className="w-[110px] text-right">Captured</TableHead>
                <TableHead className="w-[130px] text-right">Last used</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {keys.data.data.map((key) => (
                <TableRow className="cursor-pointer" key={key.api_key}>
                  <TableCell className="p-0">
                    <Link
                      className="block truncate px-3 py-2.5 font-mono text-[12.5px]"
                      search={{ api_key: key.api_key }}
                      to="/mail"
                    >
                      {key.api_key}
                    </Link>
                  </TableCell>
                  <TableCell className="p-0 text-right">
                    <Link
                      className="block px-3 py-2.5 text-[13px] tabular-nums"
                      search={{ api_key: key.api_key }}
                      to="/mail"
                    >
                      {key.captured}
                    </Link>
                  </TableCell>
                  <TableCell className="p-0 text-right">
                    <Link
                      className="block px-3 py-2.5 text-[12px] text-muted-foreground tabular-nums"
                      search={{ api_key: key.api_key }}
                      title={key.last_captured_at ? formatExact(key.last_captured_at) : undefined}
                      to="/mail"
                    >
                      {key.last_captured_at ? formatRelative(key.last_captured_at) : "never"}
                    </Link>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      ) : (
        <Empty title="No key has been used yet">
          A key appears here the first time something is sent with it. Give each test its own — that
          is what makes one instance safe for a suite running in parallel.
        </Empty>
      )}
    </Page>
  )
}
