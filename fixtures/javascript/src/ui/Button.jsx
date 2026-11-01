export function Button({ label }) {
  return <button type="button">{label}</button>;
}

export default function IconButton() {
  return <Button label="icon" />;
}
