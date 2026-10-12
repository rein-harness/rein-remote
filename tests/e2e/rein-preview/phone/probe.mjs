// Does this Chromium decode H.264 with WebCodecs? (http://localhost is a secure context.)
import http from 'node:http';
import puppeteer from 'puppeteer-core';
const srv = http.createServer((q, r) => r.end('<!doctype html><title>probe</title>')).listen(8099);
const b = await puppeteer.launch({executablePath: '/usr/bin/chromium', args: ['--no-sandbox', '--headless=new']});
const p = await b.newPage();
await p.goto('http://localhost:8099/');
console.log(await b.version(), JSON.stringify(await p.evaluate(async () => ({
  secure: isSecureContext,
  webcodecs: typeof VideoDecoder,
  avc: typeof VideoDecoder !== 'undefined' ? (await VideoDecoder.isConfigSupported({codec: 'avc1.42e01f'})).supported : null,
  avcHigh: typeof VideoDecoder !== 'undefined' ? (await VideoDecoder.isConfigSupported({codec: 'avc1.640028'})).supported : null,
  mse: typeof MediaSource !== 'undefined' && MediaSource.isTypeSupported('video/mp4; codecs="avc1.42e01f"'),
}))));
await b.close(); srv.close();
