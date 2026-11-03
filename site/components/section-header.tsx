/** Section header: a channel-style eyebrow, a display heading, then one plain lead line. */
export function SectionHeader({
  id,
  channel,
  title,
  rest,
  lead,
  className = "",
}: {
  id: string;
  channel: string;
  title: string;
  rest?: string;
  lead?: React.ReactNode;
  className?: string;
}) {
  return (
    <header className={`max-w-[46rem] ${className}`}>
      <p className="eyebrow">
        <b>#</b>
        {channel}
      </p>
      <h2 id={id} className="section-title mt-3 max-w-[22ch]">
        {title}
      </h2>
      {rest ? <p className="section-lead mt-4 max-w-[52ch]">{rest}</p> : null}
      {lead ? <div className="mt-4">{lead}</div> : null}
    </header>
  );
}
