import { Button } from './Button.jsx';
import Icon from './Button';
import { Header } from './Header';

const components = { Button };

export function Page({ kind }) {
  const Dynamic = components[kind];
  return (
    <main>
      <Header />
      <Button label="a" />
      <Icon />
      <Dynamic />
      <Missing />
      <div className="x" />
    </main>
  );
}
