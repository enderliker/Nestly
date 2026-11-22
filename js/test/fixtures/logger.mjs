const id = process.env.LOGGER_ID ?? 'child';
const count = Number(process.env.LOGGER_COUNT ?? '1000');

for (let index = 0; index < count; index += 1) {
  process.stdout.write(`${id}:${index}\n`);
}