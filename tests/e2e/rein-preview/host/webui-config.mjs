// ~/.rein/webui.json in password mode, hashed as Rein's hashPassword does (scrypt, 32 bytes, hex).
import {randomBytes, scryptSync} from 'node:crypto';
import {mkdirSync, writeFileSync} from 'node:fs';
const [name, password] = process.argv.slice(2);
const salt = randomBytes(16).toString('hex');
const hash = scryptSync(password, salt, 32).toString('hex');
mkdirSync('/root/.rein', {recursive: true, mode: 0o700});
writeFileSync('/root/.rein/webui.json', JSON.stringify({mode: 'password', host: '127.0.0.1', users: [{name, salt, hash}], createdAt: Date.now()}, null, 2) + '\n', {mode: 0o600});
