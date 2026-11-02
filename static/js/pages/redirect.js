const countdown = document.getElementById("countdown");
if (countdown) {
  const target = countdown.dataset.target;
  let seconds = Number.parseInt(countdown.dataset.seconds, 10);
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 5;
  countdown.textContent = String(seconds);
  const timer = window.setInterval(() => {
    seconds -= 1;
    countdown.textContent = String(Math.max(0, seconds));
    if (seconds <= 0) {
      window.clearInterval(timer);
      if (target) window.location.assign(target);
    }
  }, 1000);
}
