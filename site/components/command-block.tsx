import { CopyButton } from "./copy-button";

/** A terminal on the lane band. Shell lines get a "$" prompt that is not copied. */
export function CommandBlock({
  lines,
  title,
  shell = true,
}: {
  lines: readonly string[];
  title?: string;
  shell?: boolean;
}) {
  return (
    <div className="cmd lane">
      <div className="cmd-head">
        <span>{title ?? (shell ? "shell" : "")}</span>
        {shell ? <CopyButton text={lines.join("\n")} /> : null}
      </div>
      <pre className="cmd-body" tabIndex={0}>
        <code>
          {lines.map((l, i) => (
            <span key={i} className="cmd-line">
              {shell ? (
                <span className="cmd-prompt" aria-hidden="true">
                  ${" "}
                </span>
              ) : null}
              {l}
              {"\n"}
            </span>
          ))}
        </code>
      </pre>
    </div>
  );
}
