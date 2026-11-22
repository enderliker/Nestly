const mode = process.env.MODE ?? 'serve';
const code = Number(process.env.CODE ?? '0');

function stayAlive() {
  setInterval(() => {}, 1_000);
}

switch (mode) {
  case 'exit': {
    setTimeout(() => process.exit(code), 10);
    break;
  }
  case 'serve': {
    console.log('serve-ready');
    process.on('SIGTERM', () => {
      console.log('sigterm');
      process.exit(0);
    });
    process.on('SIGINT', () => {
      console.log('sigint');
      process.exit(0);
    });
    stayAlive();
    break;
  }
  case 'stubborn': {
    console.log('stubborn-ready');
    process.on('SIGTERM', () => {});
    stayAlive();
    break;
  }
  case 'echo': {
    console.log(
      `env: RUST_LOG=${process.env.RUST_LOG} SECRET_OTHER=${process.env.SECRET_OTHER}`,
    );
    process.on('SIGTERM', () => process.exit(0));
    stayAlive();
    break;
  }
  default: {
    console.error(`unknown mode ${mode}`);
    process.exit(2);
  }
}