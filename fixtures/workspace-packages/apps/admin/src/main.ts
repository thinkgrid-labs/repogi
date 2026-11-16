import { typed } from '@acme/typed';
import { built } from '@acme/built';
import { Session } from '@acme/session';
typed();
const session = new Session();
session.validate();
