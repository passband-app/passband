# THE TEST BED'S MTA (scripts/imap-dev.sh). DEV ONLY. Dovecot's submission
# service (the port the daemon talks to, with TLS and AUTH) hands each message
# to this Postfix, which delivers it back into Dovecot over LMTP. It has no
# published port and relays nowhere: anything not for @dev.test bounces.
FROM alpine:3.22.2
RUN apk add --no-cache 'postfix~3.10'
COPY main.cf.append /etc/postfix/main.cf.append
RUN cat /etc/postfix/main.cf.append >> /etc/postfix/main.cf && postfix check
CMD ["postfix", "start-fg"]
