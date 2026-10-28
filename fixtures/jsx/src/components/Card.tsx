export default function Card({ children }: { children?: unknown }) {
  return <div className="card">{children as string}</div>;
}
