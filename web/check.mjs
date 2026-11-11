import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const files=[new URL('serve.mjs',import.meta.url),...fs.readdirSync(new URL('dist/',import.meta.url)).filter(name=>name.endsWith('.js')).map(name=>new URL(`dist/${name}`,import.meta.url))];
for(const file of files){
  const result=spawnSync(process.execPath,['--check',fileURLToPath(file)],{stdio:'inherit',windowsHide:true});
  if(result.error)throw result.error;
  if(result.status!==0)process.exit(result.status||1);
}
console.log('Web runtime syntax: ok');
