import "dotenv/config";
import "newrelic";

import fetch from "node-fetch";
import express from "express";

const app = express();

app.get("/", async (req, res) => {
  // This request will have w3c trace context headers.
  await fetch("http://127.0.0.1:3001");
  res.status(204).end();
});

app.listen(3000);
