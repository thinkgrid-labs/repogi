import { Button } from './components/Button';
import { Panel } from '@/widgets/Panel';
import { Button as PrimaryButton } from './components/Button';
import Card from './components/Card';
import { Card as BarrelCard } from '@/components';
import * as Icons from './components/Icons';
import { Toaster } from 'sonner';
import { Ambig } from './Ambig';
import type { TypedThing } from './types';
import { PanelProps } from './types';
import { Shadowed } from './components/Shadowed';

export function App() {
  return (
    <Panel>
      <Button />
      <PrimaryButton></PrimaryButton>
      <Card>
        <span>text that says Button</span>
      </Card>
      <BarrelCard />
      <Icons.Star />
      <Toaster />
      <Ambig />
      <TypedThing />
      <my-element data-x={Button} />
    </Panel>
  );
}

export function Shadowing() {
  const Shadowed = () => <i>local</i>;
  return <Shadowed />;
}

export const useRenderer = () => {
  const render = () => <Button />;
  return render;
};

export function TypeAsTag() {
  return <PanelProps />;
}

export function WithDefault({ renderEmpty = () => <Card /> }: { renderEmpty?: () => unknown }) {
  return renderEmpty();
}

import { ThemeContext } from './context';

export function Themed() {
  return (
    <ThemeContext.Provider value="dark">
      <Button />
    </ThemeContext.Provider>
  );
}
