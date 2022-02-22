//
// Created by albert on 2022-02-22.
//

#include "nada-controller.h"
using rmcat::NadaController;

NadaController* NewController() {
    return new NadaController();
}

void FreeController(NadaController* c) {
    delete c;
}

void OnFeedback(NadaController* controller, uint64_t nowUs,
              uint16_t sequence,
              uint64_t rxTimestampUs,
              uint8_t ecn) {
    controller->processFeedback(nowUs, sequence, rxTimestampUs, ecn);
}

void OnPacket(NadaController* controller, uint64_t txTimestampUs,
              uint16_t sequence,
              uint32_t size) {
    controller->processFeedback(txTimestampUs, sequence, size);
}

float getBitrate(NadaController* controller, uint64_t nowUs) {
    return controller->getBandwidth(nowUs);
}