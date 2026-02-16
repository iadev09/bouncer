# TODO

- Add Postfix `delay_notice_recipient` and `error_notice_recipient` ingestion paths.
- Map these notice types into DB delivery statuses (e.g., delayed, error) in the processing layer.
- Review related Postfix notice settings (`2bounce_notice_recipient`, `notify_classes`) during integration.
- journald logging integration for observer and server

#  client test 
```textmate
printf 'Subject: dev\n\nhello' | cargo run -p bouncer-client -- --server 127.0.0.1:2147 --from sender@example.com --to bounces@example.com
```
