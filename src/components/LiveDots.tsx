/** The reference's three-dot "quiet" activity wave, used while a turn is live. */
export default function LiveDots({ label }: { label?: string }) {
  return (
    <span className="live-dots" role="status" aria-label={label ?? '处理中'}>
      <span className="timeline-quiet-dots" aria-hidden="true">
        <span />
        <span />
        <span />
      </span>
      {label && <span className="live-dots-label">{label}</span>}
    </span>
  );
}
