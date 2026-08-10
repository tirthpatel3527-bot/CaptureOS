import type { PropsWithChildren } from "react";

export function StatusCard({ children }: PropsWithChildren) {
  return <section className="status-card">{children}</section>;
}
