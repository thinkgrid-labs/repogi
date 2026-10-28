import { Toaster } from 'sonner';
import { formatDate } from '../lib/format';

export function Report({ at }: { at: Date }) {
  return (
    <p>
      {formatDate(at)}
      <Toaster />
    </p>
  );
}
