# Specification for communication system protocol.

## Messages format

```
<MESSAGE_KIND><MESSAGE_UUID><PAYLOAD>
```

## Fields

Field          | Length             | Description                                   |
:-------------:|:------------------:|:---------------------------------------------:|
`MESSAGE_KIND` | 4 bytes            | Kind of message. Enumeration of all exist messages kind can be found below |
`MESSAGE_UUID` | 16 bytes           | Message universally unique identifier (UUID). |
`PAYLOAD`      | any count of bytes | Message content in JSON format.               |

## Enumeration of interfaces for messages content.
